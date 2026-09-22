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
use std::time::Duration;

use futures::SinkExt;
use rustak_cot::codec::{EncodedEvent, MAX_PROTO_PAYLOAD, Mode, TakCodec};
use rustak_cot::detail::fileshare::FileShare;
use rustak_cot::error::CodecError;
use rustak_cot::event::Point;
use rustak_cot::types::{cot_type, how};
use rustak_cot::{CotTime, Event};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio_util::codec::FramedWrite;

use crate::prelude::*;

use super::liveness::{LeaveReason, Liveness};
use super::metrics::StreamMetrics;
use super::subscription::Outbound;

/// How many queued messages one flush may carry.
const DRAIN_BURST: usize = 64;

/// How long the **server-generated** `b-f-t-r` substitute stays valid.
///
/// A hundred seconds, not the ten that `rustak_cot::detail::fileshare` uses.
/// Research `05` §9 pins two different templates: ATAK's own client-emitted
/// offer (`+10s`, with an `<ackrequest>`) and `CommonUtil.getFileTransferCotMessage`
/// (`+100s`, no `<ackrequest>`, `hae='9999999.0'`), and it is the **second**
/// that stands in for an oversize outbound message. Ten seconds is the whole
/// window ATAK has to notice the pointer and start the fetch; at a hundred it
/// has ten times as long. R-02 M1.
const SUBSTITUTE_VALIDITY: std::time::Duration = std::time::Duration::from_secs(100);

/// The callsign a substitute carries when the original event had none.
///
/// TAK Server uses the authenticated user's name or this literal; an empty
/// `senderCallsign` is what the receiving client would otherwise display.
const SERVER_CALLSIGN: &str = "takserver";

/// What the writer needs beyond its queue and its socket.
#[derive(Clone, Debug)]
pub struct WriterContext {
    /// The listener's counters.
    pub metrics: Arc<StreamMetrics>,
    /// The base URL an oversize message's pointer sends a client to. Without
    /// one there is nowhere to point, and an oversize message is dropped.
    pub public_url: Option<String>,
    /// How long one write may take before the peer is given up on.
    ///
    /// `[stream.limits] write_timeout`. See [`run`].
    pub write_timeout: Duration,
    /// The connection's clocks and cause of death.
    ///
    /// The writer is the only task that knows when a write *completed*, which
    /// is half of the idle rule (`liveness`), and the only one that can name a
    /// peer that stopped taking bytes.
    pub liveness: Arc<Liveness>,
}

impl WriterContext {
    /// Records why this connection is ending, and reports it.
    fn ended(&self, reason: LeaveReason) -> LeaveReason {
        self.liveness.ended(reason)
    }
}

/// Runs a connection's writer until its queue closes or the socket fails.
///
/// # A half-dead peer does not get to keep a socket
///
/// Two bounds, because the writer can stall in two different places (R-03 H1).
///
/// It can stall **waiting**: `rx.recv()` only ends when every sender has been
/// dropped, and a `ConnHandle` clone held by something mid-route keeps one
/// alive. `shutdown` — the connection's own token, cancelled by
/// [`ConnHandle::close`] — ends that wait.
///
/// It can stall **writing**: a peer that completed the handshake and then
/// stopped reading leaves `flush()` parked on a socket whose window is zero,
/// with no timeout of its own and nothing to interrupt it. Because
/// `tokio::io::split` closes the socket only when *both* halves drop, that one
/// task holds the file descriptor and the TLS session for the life of the
/// process. `context.write_timeout` is the deadline on each write, and
/// `connection::run` aborts whatever is left after the drain.
///
/// [`ConnHandle::close`]: super::subscription::ConnHandle::close
pub async fn run<W: AsyncWrite + Unpin>(
    mut rx: mpsc::Receiver<Outbound>,
    mut sink: FramedWrite<W, TakCodec>,
    context: WriterContext,
    shutdown: Shutdown,
) {
    let mut batch: Vec<Outbound> = Vec::with_capacity(DRAIN_BURST);

    while let Some(first) = next(&mut rx, &shutdown).await {
        batch.push(first);

        while batch.len() < DRAIN_BURST {
            match rx.try_recv() {
                Ok(next) => batch.push(next),
                Err(_) => break,
            }
        }

        for outbound in batch.drain(..) {
            if !write(&mut sink, outbound, &context).await {
                close(&mut sink, context.write_timeout).await;
                // The read side has nothing to wait for on a socket that will
                // not take bytes: without this the connection would sit there
                // until the idle timeout, still registered and still being
                // routed to. `Outbound::Close` arrives here already cancelled,
                // so this only ever ends a connection that was going anyway.
                shutdown.cancel();

                return;
            }
        }

        if !flush(&mut sink, &context).await {
            shutdown.cancel();

            return;
        }

        // The flush returned, so these bytes have left this process: the
        // connection is not idle, however long the client has been silent.
        context.liveness.wrote();
    }

    // The queue closed, which is the connection task saying it has finished.
    flush(&mut sink, &context).await;
    close(&mut sink, context.write_timeout).await;
}

/// The next thing to write, or [`None`] when there will not be another.
///
/// A cancelled connection still writes whatever is *already* queued: the close
/// is a statement about the future, and the router counted those messages as
/// delivered a moment ago. What cancellation ends is the **wait** — see [`run`].
async fn next(rx: &mut mpsc::Receiver<Outbound>, shutdown: &Shutdown) -> Option<Outbound> {
    if shutdown.is_cancelled() {
        return rx.try_recv().ok();
    }

    tokio::select! {
        next = rx.recv() => next,
        () = shutdown.cancelled() => rx.try_recv().ok(),
    }
}

/// Flushes the framer, giving up on a socket that will not take the bytes.
///
/// `TakCodec` encodes two different item types, so `SinkExt::flush` cannot work
/// out which `Sink` implementation is meant; the fan-out one is named here
/// once rather than at every call site.
///
/// `false` means stop.
async fn flush<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, TakCodec>,
    context: &WriterContext,
) -> bool {
    let within = context.write_timeout;
    let flushing = <FramedWrite<W, TakCodec> as SinkExt<&EncodedEvent>>::flush(sink);

    match tokio::time::timeout(within, flushing).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            context.ended(LeaveReason::WriteError);
            debug!(error = %err, "A stream connection's writer could not flush.");

            false
        }
        Err(_) => {
            context.ended(LeaveReason::WriteTimeout);
            debug!(
                seconds = within.as_secs(),
                "A stream connection stopped accepting writes and was given up on."
            );

            false
        }
    }
}

/// Flushes and shuts the framer down, ignoring a socket that has already gone.
async fn close<W: AsyncWrite + Unpin>(sink: &mut FramedWrite<W, TakCodec>, within: Duration) {
    let closing = <FramedWrite<W, TakCodec> as SinkExt<&EncodedEvent>>::close(sink);

    let _ = tokio::time::timeout(within, closing).await;
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

            if !flush(sink, context).await {
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

                fed(sink.feed(&pointer), context).await
            }
            None => true,
        };
    }

    fed(sink.feed(encoded.as_ref()), context).await
}

/// Waits for one `feed`, under the same deadline the flush is held to.
///
/// `feed` buffers rather than writing, but `FramedWrite` writes through once
/// its buffer is past the high-water mark — so on a socket that has stopped
/// draining this awaits too, and needs the deadline just as much.
async fn fed(
    feeding: impl std::future::Future<Output = Result<(), CodecError>>,
    context: &WriterContext,
) -> bool {
    let within = context.write_timeout;

    match tokio::time::timeout(within, feeding).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            context.ended(LeaveReason::WriteError);
            debug!(error = %err, "A stream connection's writer could not encode a message.");

            false
        }
        Err(_) => {
            context.ended(LeaveReason::WriteTimeout);
            debug!(
                seconds = within.as_secs(),
                "A stream connection stopped accepting writes and was given up on."
            );

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
        // Served by `marti::cot::single`. Until R-02 H1 this pointed at a route
        // that did not exist, so the substitution lost the message and showed
        // the user a failed transfer.
        sender_url: format!(
            "{}/Marti/api/cot/xml/{}",
            base.trim_end_matches('/'),
            event.uid
        ),
        size_in_bytes: xml.len() as u64,
        sha256: hex::encode(Sha256::digest(xml)),
        sender_uid: event.uid.clone(),
        sender_callsign: event
            .callsign()
            .filter(|callsign| !callsign.is_empty())
            .unwrap_or(SERVER_CALLSIGN)
            .to_owned(),
        name: event.uid.clone(),
        ..FileShare::default()
    };

    debug!(
        uid = %event.uid,
        bytes = encoded.len(Mode::Proto),
        "Substituted a pointer for a message too large to frame."
    );

    Some(EncodedEvent::new(pointer(&event.uid, &share)))
}

/// The server-generated `b-f-t-r`, per research `05` §9.
///
/// Built here rather than with `rustak_cot::detail::fileshare::fileshare_pointer`
/// because that helper renders **ATAK's** template — `stale=+10s`, an
/// unconditional `<ackrequest>` and `hae="0.0"` — and this is the server's, which
/// differs in all three. The two are genuinely different messages that happen to
/// share a type, and `compat/files.md` §9 conflating them is what R-02 M1 found.
fn pointer(uid: &str, share: &FileShare) -> Event {
    Event::builder(cot_type::FILESHARE, uid)
        .how(how::H_E)
        // `hae` is the unknown sentinel, not sea level: a receiver plotting
        // `hae="0.0"` puts the pointer on the surface of the ellipsoid.
        .point_full(Point {
            lat: 0.0,
            lon: 0.0,
            hae: Point::UNKNOWN_HAE,
            ce: Point::UNKNOWN_CE,
            le: Point::UNKNOWN_LE,
        })
        .time(CotTime::now())
        .stale_after(SUBSTITUTE_VALIDITY)
        // No `<ackrequest>`: the server is not waiting for a receipt, and ATAK
        // would answer a `b-f-t-a` that nothing here consumes.
        .typed(share)
        .build()
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
            write_timeout: Duration::from_secs(5),
            liveness: Arc::new(Liveness::new()),
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

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_stops_reading_is_given_up_on_and_named_as_a_write_timeout() {
        // R-03 H1. The socket takes sixteen bytes and its peer never reads, so
        // the flush parks with nowhere to put the rest. Without a deadline the
        // task, the file descriptor and the TLS session behind it survive for
        // the life of the process — and this test does not terminate.
        //
        // The clock is paused, so what is asserted is the *order* — the writer
        // stops, names the cause and cancels the connection — rather than how
        // long any of it took on this host. M9-15: the cause is what turns the
        // disconnect line from "a client left" into something an operator can
        // act on.
        let (client, server) = tokio::io::duplex(16);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));
        let closing = Shutdown::new();

        let mut context = context(None);
        context.write_timeout = Duration::from_millis(100);
        let liveness = Arc::clone(&context.liveness);

        for uid in 0..8 {
            tx.send(Outbound::Event(event(&format!("UID-{uid}"))))
                .await
                .unwrap();
        }
        drop(tx);

        run(rx, sink, context, closing.clone()).await;

        assert_eq!(
            liveness.reason(),
            Some(LeaveReason::WriteTimeout),
            "the peer stopped taking bytes; that is what the disconnect must say",
        );
        assert!(
            closing.is_cancelled(),
            "the read side must not wait out the idle timeout on a socket that \
             will not take bytes",
        );

        // Held to the end, so the socket is not closed from under the writer
        // and the test is about the deadline rather than about an error.
        drop(client);
    }

    #[tokio::test(start_paused = true)]
    async fn a_completed_flush_is_what_keeps_a_quiet_connection_alive() {
        // The other half of M9-15: the idle clock is `max(last_rx, last_tx)`,
        // and this is where `last_tx` moves. A client that says nothing while
        // the server writes to it must not look idle.
        let (client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));
        let context = context(None);
        let liveness = Arc::clone(&context.liveness);

        let idle = Duration::from_secs(90);
        let before = liveness.idle_deadline(idle);

        // Half an hour of a client saying nothing, and then one message
        // written to it.
        tokio::time::advance(Duration::from_secs(1_800)).await;
        tx.send(Outbound::Event(event("UID-1"))).await.unwrap();
        drop(tx);
        run(rx, sink, context, Shutdown::new()).await;

        assert_eq!(
            liveness.idle_deadline(idle) - before,
            Duration::from_secs(1_800),
            "a completed write pushes the idle deadline out by exactly as long \
             as the connection had been quiet",
        );
        assert_eq!(liveness.reason(), None, "nothing went wrong here");

        drop(client);
    }

    #[tokio::test]
    async fn a_closed_connection_stops_the_writer_even_with_a_sender_still_held() {
        // The other half of H1: `rx.recv()` only ends when *every* sender has
        // been dropped, and a `ConnHandle` clone held by something mid-route
        // keeps one alive. The shutdown arm is what ends the wait.
        let (_client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));
        let closing = Shutdown::new();

        let writing = tokio::spawn(run(rx, sink, context(None), closing.clone()));

        // Queued before the close, and still written: the close is a statement
        // about the future, and the router counted this one as delivered.
        tx.send(Outbound::Event(event("UID-LAST"))).await.unwrap();
        closing.cancel();

        tokio::time::timeout(Duration::from_secs(5), writing)
            .await
            .expect("the writer stops without waiting for the last sender")
            .expect("and does not panic");

        drop(tx);
    }

    #[tokio::test]
    async fn messages_are_written_in_the_order_they_were_queued() {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (tx, rx) = mpsc::channel(8);
        let sink = FramedWrite::new(server, TakCodec::new(Mode::Xml));

        tx.send(Outbound::Event(event("UID-1"))).await.unwrap();
        tx.send(Outbound::Event(event("UID-2"))).await.unwrap();
        drop(tx);

        run(rx, sink, context(None), Shutdown::new()).await;

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

        run(rx, sink, context(None), Shutdown::new()).await;

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

        run(rx, sink, context.clone(), Shutdown::new()).await;

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

        // The server-generated template, not ATAK's own offer — research 05 §9,
        // R-02 M1.
        assert_eq!(
            pointer.stale,
            pointer.time.stale_after(SUBSTITUTE_VALIDITY),
            "the substitute is valid for 100s, which is how long ATAK has to fetch it",
        );
        assert!(
            pointer.detail.find("ackrequest").is_none(),
            "the server-generated template has no <ackrequest>; ATAK would answer \
             a b-f-t-a nothing consumes",
        );
        assert_eq!(
            pointer.point.hae,
            Point::UNKNOWN_HAE,
            "hae=0.0 would plot the pointer at sea level rather than at unknown altitude",
        );
        assert_eq!(
            share.sender_callsign, "takserver",
            "an event with no callsign still names a sender",
        );
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

        run(
            rx,
            sink,
            context(Some("https://tak.example.com")),
            Shutdown::new(),
        )
        .await;

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

        run(rx, sink, context(None), Shutdown::new()).await;

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

        run(rx, sink, context(None), Shutdown::new()).await;

        let mut reader = FramedRead::new(client, TakCodec::new(Mode::Xml));
        assert!(
            reader.next().await.is_none(),
            "nothing queued after a close is written",
        );
    }
}
