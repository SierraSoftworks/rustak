//! The half of a connection that writes.
//!
//! One task per connection, reading from that connection's bounded queue. It
//! exists so that routing never touches a socket: the sending client's task
//! hands an `Arc` to every recipient's queue and moves on, and each recipient's
//! writer pays for its own slow network in its own time.
//!
//! # Why it drains greedily
//!
//! A burst of position reports arrives as a burst, and writing each one with
//! its own flush would mean one syscall per message per recipient. The writer
//! takes whatever is already queued, feeds it all into the framer, and flushes
//! once — so a fan-out of fifty messages to one client costs one write.
//!
//! # The mode switch goes through here
//!
//! [`Outbound::SwitchToProto`] carries the `t-x-takp-r` answer, so the answer
//! and the switch that follows it are one step on one task. Nothing else can
//! interleave between them, which is what guarantees the client never sees XML
//! after the message that told it to stop expecting any.

use std::sync::Arc;

use futures::SinkExt;
use rustak_cot::codec::{EncodedEvent, MAX_PROTO_PAYLOAD, Mode, TakCodec};
use rustak_cot::detail::fileshare::{FileShare, fileshare_pointer};
use rustak_cot::error::CodecError;
use rustak_cot::{CotTime, Event};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio_util::codec::FramedWrite;

use crate::prelude::*;

use super::metrics::StreamMetrics;
use super::subscription::Outbound;

/// How many queued messages one flush may carry.
const DRAIN_BURST: usize = 64;

/// What the writer needs beyond its queue and its socket.
#[derive(Clone, Debug)]
pub struct WriterContext {
    /// The listener's counters.
    pub metrics: Arc<StreamMetrics>,
    /// The base URL an oversize message's pointer sends a client to. Without
    /// one there is nowhere to point, and an oversize message is dropped.
    pub public_url: Option<String>,
}

/// Runs a connection's writer until its queue closes or the socket fails.
pub async fn run<W: AsyncWrite + Unpin>(
    mut rx: mpsc::Receiver<Outbound>,
    mut sink: FramedWrite<W, TakCodec>,
    context: WriterContext,
) {
    let mut batch: Vec<Outbound> = Vec::with_capacity(DRAIN_BURST);

    while let Some(first) = rx.recv().await {
        batch.push(first);

        while batch.len() < DRAIN_BURST {
            match rx.try_recv() {
                Ok(next) => batch.push(next),
                Err(_) => break,
            }
        }

        for outbound in batch.drain(..) {
            if !write(&mut sink, outbound, &context).await {
                close(&mut sink).await;

                return;
            }
        }

        if let Err(err) = flush(&mut sink).await {
            debug!(error = %err, "A stream connection's writer could not flush.");

            return;
        }
    }

    // The queue closed, which is the connection task saying it has finished.
    let _ = flush(&mut sink).await;
    close(&mut sink).await;
}

/// Flushes the framer.
///
/// `TakCodec` encodes two different item types, so `SinkExt::flush` cannot work
/// out which `Sink` implementation is meant; the fan-out one is named here
/// once rather than at every call site.
async fn flush<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, TakCodec>,
) -> Result<(), CodecError> {
    <FramedWrite<W, TakCodec> as SinkExt<&EncodedEvent>>::flush(sink).await
}

/// Flushes and shuts the framer down, ignoring a socket that has already gone.
async fn close<W: AsyncWrite + Unpin>(sink: &mut FramedWrite<W, TakCodec>) {
    let _ = <FramedWrite<W, TakCodec> as SinkExt<&EncodedEvent>>::close(sink).await;
}

/// Writes one queued item. `false` means stop.
async fn write<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, TakCodec>,
    outbound: Outbound,
    context: &WriterContext,
) -> bool {
    match outbound {
        Outbound::Event(encoded) => feed(sink, &encoded, context).await,
        Outbound::SwitchToProto(answer) => {
            // The answer is the last XML this socket will ever carry, so it is
            // written and flushed before the codec changes.
            if !feed(sink, &answer, context).await {
                return false;
            }

            if let Err(err) = flush(sink).await {
                debug!(error = %err, "Could not flush the protocol answer.");

                return false;
            }

            sink.encoder_mut().set_mode(Mode::Proto);

            true
        }
        Outbound::Close => false,
    }
}

/// Feeds one message, substituting a pointer when it is too large to send.
async fn feed<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, TakCodec>,
    encoded: &Arc<EncodedEvent>,
    context: &WriterContext,
) -> bool {
    let mode = sink.encoder().mode();

    if mode == Mode::Proto && encoded.len(Mode::Proto) > MAX_PROTO_PAYLOAD {
        return match substitute(encoded, context) {
            Some(pointer) => {
                StreamMetrics::incr(&context.metrics.oversize_substituted);

                sink.feed(&pointer).await.is_ok()
            }
            None => true,
        };
    }

    match sink.feed(encoded.as_ref()).await {
        Ok(()) => true,
        Err(err) => {
            debug!(error = %err, "A stream connection's writer could not encode a message.");

            false
        }
    }
}

/// Builds the `b-f-t-r` pointer that stands in for an oversize message.
///
/// ATAK's receive buffer is 64 KiB and it resynchronises rather than growing
/// one, so a frame past that is a frame it will never read. Pointing it at
/// `/Marti/api/cot/xml/{uid}` lets it fetch the message over HTTP instead,
/// which is what TAK Server does and what the client already knows how to do
/// (`compat/streaming.md` §3).
fn substitute(encoded: &EncodedEvent, context: &WriterContext) -> Option<EncodedEvent> {
    let event = encoded.event();

    let Some(base) = &context.public_url else {
        warn!(
            uid = %event.uid,
            bytes = encoded.len(Mode::Proto),
            "Dropped an oversize message: no public URL is configured to point a client at."
        );

        return None;
    };

    let xml = encoded.xml();
    let share = FileShare {
        filename: format!("{}.xml", event.uid),
        sender_url: format!(
            "{}/Marti/api/cot/xml/{}",
            base.trim_end_matches('/'),
            event.uid
        ),
        size_in_bytes: xml.len() as u64,
        sha256: hex::encode(Sha256::digest(xml)),
        sender_uid: event.uid.clone(),
        sender_callsign: event.callsign().unwrap_or_default().to_owned(),
        name: event.uid.clone(),
        ..FileShare::default()
    };

    let pointer: Event = fileshare_pointer(&event.uid, &share, CotTime::now());

    debug!(
        uid = %event.uid,
        bytes = encoded.len(Mode::Proto),
        "Substituted a pointer for a message too large to frame."
    );

    Some(EncodedEvent::new(pointer))
}

#[cfg(test)]
mod tests {
    use rustak_cot::detail::Element;
    use rustak_cot::types::cot_type;
    use rustak_cot::{codec::Frame, xml};
    use tokio_util::codec::FramedRead;

    use super::*;
    use futures::StreamExt;

    fn context(public_url: Option<&str>) -> WriterContext {
        WriterContext {
            metrics: Arc::new(StreamMetrics::default()),
            public_url: public_url.map(str::to_owned),
        }
    }

    fn event(uid: &str) -> Arc<EncodedEvent> {
        Arc::new(EncodedEvent::new(
            Event::builder("a-f-G-U-C", uid).point(51.5, -0.12).build(),
        ))
    }

    /// An event whose protobuf form is comfortably past the 64 KiB ceiling.
    fn huge(uid: &str) -> Arc<EncodedEvent> {
        let remarks = Element::new("remarks").with(rustak_cot::Node::Text("x".repeat(70_000)));

        Arc::new(EncodedEvent::new(
            Event::builder("b-t-f", uid)
                .point(51.5, -0.12)
                .push(remarks)
                .build(),
        ))
    }

    #[tokio::test]
    async fn messages_are_written_in_the_order_they_were_queued() {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));

        tx.send(Outbound::Event(event("UID-1"))).await.unwrap();
        tx.send(Outbound::Event(event("UID-2"))).await.unwrap();
        drop(tx);

        run(rx, sink, context(None)).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Xml));
        let uids: Vec<String> = vec![
            reader.next().await.unwrap().unwrap(),
            reader.next().await.unwrap().unwrap(),
        ]
        .into_iter()
        .map(|frame| xml::parse(frame.payload()).unwrap().uid)
        .collect();

        assert_eq!(uids, vec!["UID-1".to_string(), "UID-2".to_string()]);
    }

    #[tokio::test]
    async fn the_answer_is_the_last_xml_and_everything_after_it_is_protobuf() {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));

        let answer = Arc::new(EncodedEvent::new(rustak_cot::negotiate::response(
            "neg-1",
            true,
            CotTime::now(),
        )));
        tx.send(Outbound::SwitchToProto(answer)).await.unwrap();
        tx.send(Outbound::Event(event("UID-1"))).await.unwrap();
        drop(tx);

        run(rx, sink, context(None)).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Xml));
        let first = reader.next().await.unwrap().unwrap();
        assert!(matches!(first, Frame::Xml(_)));
        assert_eq!(
            xml::parse(first.payload()).unwrap().r#type,
            cot_type::TAKP_R
        );

        reader.decoder_mut().set_mode(Mode::Proto);
        let second = reader.next().await.unwrap().unwrap();
        assert!(matches!(second, Frame::Proto(_)));
    }

    #[tokio::test]
    async fn an_oversize_message_becomes_a_pointer_a_client_can_fetch() {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Proto));
        let context = context(Some("https://tak.example.com:8443"));

        let oversize = huge("UID-BIG");
        assert!(oversize.len(Mode::Proto) > MAX_PROTO_PAYLOAD);
        tx.send(Outbound::Event(oversize)).await.unwrap();
        drop(tx);

        run(rx, sink, context.clone()).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Proto));
        let frame = reader.next().await.unwrap().unwrap();
        let message = rustak_cot::proto::decode(frame.payload()).unwrap();
        let pointer = rustak_cot::proto::message_to_event(message).unwrap();

        assert_eq!(pointer.r#type, cot_type::FILESHARE);
        let share = pointer.detail.get::<FileShare>().expect("a <fileshare>");
        assert_eq!(
            share.sender_url,
            "https://tak.example.com:8443/Marti/api/cot/xml/UID-BIG"
        );
        assert_eq!(share.sha256.len(), 64);
        assert_eq!(StreamMetrics::get(&context.metrics.oversize_substituted), 1);
    }

    #[tokio::test]
    async fn an_xml_connection_is_never_given_a_pointer() {
        // The 64 KiB ceiling is a protobuf frame limit; the XML reader has an
        // 8 MiB one, so substituting would lose a message that would have
        // arrived intact.
        let (client, server) = tokio::io::duplex(1 << 21);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));

        tx.send(Outbound::Event(huge("UID-BIG"))).await.unwrap();
        drop(tx);

        run(rx, sink, context(Some("https://tak.example.com"))).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Xml));
        let frame = reader.next().await.unwrap().unwrap();

        assert_eq!(xml::parse(frame.payload()).unwrap().uid, "UID-BIG");
    }

    #[tokio::test]
    async fn an_oversize_message_with_nowhere_to_point_is_dropped_rather_than_framed() {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Proto));

        tx.send(Outbound::Event(huge("UID-BIG"))).await.unwrap();
        tx.send(Outbound::Event(event("UID-SMALL"))).await.unwrap();
        drop(tx);

        run(rx, sink, context(None)).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Proto));
        let frame = reader.next().await.unwrap().unwrap();
        let message = rustak_cot::proto::decode(frame.payload()).unwrap();

        assert_eq!(
            rustak_cot::proto::message_to_event(message).unwrap().uid,
            "UID-SMALL",
            "the oversize message is skipped and the connection carries on",
        );
    }

    #[tokio::test]
    async fn a_close_ends_the_writer() {
        let (client, server) = tokio::io::duplex(1024);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));

        tx.send(Outbound::Close).await.unwrap();
        tx.send(Outbound::Event(event("UID-1"))).await.unwrap();

        run(rx, sink, context(None)).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Xml));
        assert!(
            reader.next().await.is_none(),
            "nothing queued after a close is written",
        );
    }
}
