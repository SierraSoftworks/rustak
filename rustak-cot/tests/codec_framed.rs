//! The codec driven the way a connection drives it: a real duplex socket, a
//! `Framed` on each end, and a mid-stream switch from XML to protobuf.
//!
//! These tests exist because the unit tests feed the scanners a `BytesMut` by
//! hand, which hides the two things that actually go wrong on a socket: reads
//! that split a message anywhere, and a mode switch that has to hand the *same*
//! buffer to a different framer without losing the bytes already in it.

use std::time::Duration;

use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use rustak_cot::codec::{EncodedEvent, Frame, MAX_MESSAGE, Mode, TakCodec};
use rustak_cot::{CotTime, Event, negotiate, proto, xml};
use tokio_util::codec::{Decoder, Framed};

/// `2026-09-17T12:00:00.000Z` — the instant the golden fixtures use, so these
/// events are byte-for-byte reproducible.
const NOW: CotTime = CotTime::from_millis(1_789_646_400_000);

fn sa(uid: &str, callsign: &str) -> Event {
    use rustak_cot::detail::{Contact, Group};
    Event::builder("a-f-G-U-C", uid)
        .how("m-g")
        .point(51.5074, -0.1278)
        .time(NOW)
        .stale_after(Duration::from_secs(120))
        .typed(&Contact::new(callsign).with_endpoint("*:-1:stcp"))
        .typed(&Group::new("Cyan", "Team Member"))
        .build()
}

/// Turns a frame back into an event, whichever encoding it is in — the step a
/// connection performs before routing.
fn event_of(frame: &Frame) -> Event {
    match frame {
        Frame::Xml(bytes) => xml::parse(bytes).expect("a framed message parses"),
        Frame::Proto(payload) => {
            let message = proto::decode(payload).expect("a framed payload decodes");
            proto::message_to_event(message).expect("the payload carries a cotEvent")
        }
    }
}

#[tokio::test]
async fn a_connection_negotiates_then_switches_both_directions_to_protobuf() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = Framed::new(server_io, TakCodec::new(Mode::Xml));
    let mut client = Framed::new(client_io, TakCodec::new(Mode::Xml));

    // 1. The server replays a peer's latest SA, then makes its one offer.
    let replay = EncodedEvent::new(sa("UID-BRAVO", "BRAVO"));
    server.send(&replay).await.expect("write the replay");
    let offer = EncodedEvent::new(negotiate::announce(
        "NEG-UID",
        "TAK Server rustak-0.1.0",
        negotiate::API_VERSION,
        NOW,
    ));
    server.send(&offer).await.expect("write the offer");

    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(event_of(&seen).callsign(), Some("BRAVO"));

    let seen = client.next().await.expect("a frame").expect("well framed");
    let announced = negotiate::parse_announce(&event_of(&seen)).expect("an offer");
    assert!(announced.supports(negotiate::PROTO_VERSION));
    assert_eq!(
        announced.server_version.as_deref(),
        Some("TAK Server rustak-0.1.0")
    );

    // 2. The client asks, reusing the offer's uid, and then stays quiet.
    let request = EncodedEvent::new(negotiate::request("NEG-UID", negotiate::PROTO_VERSION, NOW));
    client.send(&request).await.expect("write the request");

    let seen = server.next().await.expect("a frame").expect("well framed");
    let asked = negotiate::parse_request(&event_of(&seen)).expect("a readable request");
    assert_eq!(asked, negotiate::PROTO_VERSION);

    // 3. The server answers in XML and switches immediately afterwards.
    let response = EncodedEvent::new(negotiate::response("NEG-UID", true, NOW));
    server.send(&response).await.expect("write the response");
    server.codec_mut().set_mode(Mode::Proto);

    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(seen.mode(), Mode::Xml, "the response is the last XML frame");
    assert_eq!(
        negotiate::parse_response(&event_of(&seen)),
        Some(true),
        "the client switches only on an explicit true"
    );
    client.codec_mut().set_mode(Mode::Proto);

    // 4. Everything after that is protobuf, in both directions.
    let from_server = EncodedEvent::new(sa("UID-BRAVO", "BRAVO"));
    server.send(&from_server).await.expect("write an event");
    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(seen.mode(), Mode::Proto);
    assert_eq!(seen, Frame::Proto(from_server.proto().clone()));
    assert_eq!(event_of(&seen).callsign(), Some("BRAVO"));

    let from_client = EncodedEvent::new(sa("UID-ALPHA", "ALPHA"));
    client.send(&from_client).await.expect("write an event");
    let seen = server.next().await.expect("a frame").expect("well framed");
    assert_eq!(seen.mode(), Mode::Proto);
    assert_eq!(event_of(&seen).callsign(), Some("ALPHA"));
}

#[tokio::test]
async fn a_client_that_never_asks_stays_on_xml_forever() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = Framed::new(server_io, TakCodec::new(Mode::Xml));
    let mut client = Framed::new(client_io, TakCodec::new(Mode::Xml));

    let offer = EncodedEvent::new(negotiate::announce(
        "NEG-UID",
        "rustak-0.1.0",
        negotiate::API_VERSION,
        NOW,
    ));
    server.send(&offer).await.expect("write the offer");
    let _ = client.next().await.expect("a frame").expect("well framed");

    // CloudTAK's behaviour: it reads the offer, keeps the server version, and
    // never answers. The server must keep speaking XML.
    for callsign in ["ALPHA", "BRAVO", "CHARLIE"] {
        let event = EncodedEvent::new(sa("UID-A", callsign));
        server.send(&event).await.expect("write an event");
        let seen = client.next().await.expect("a frame").expect("well framed");
        assert_eq!(seen.mode(), Mode::Xml);
        assert_eq!(event_of(&seen).callsign(), Some(callsign));
    }
    assert_eq!(server.codec().mode(), Mode::Xml);
}

#[tokio::test]
async fn a_refused_request_leaves_both_ends_on_xml() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = Framed::new(server_io, TakCodec::new(Mode::Xml));
    let mut client = Framed::new(client_io, TakCodec::new(Mode::Xml));

    // A client asking for a version we do not speak.
    let request = EncodedEvent::new(negotiate::request("NEG-UID", 2, NOW));
    client.send(&request).await.expect("write the request");
    let seen = server.next().await.expect("a frame").expect("well framed");
    let asked = negotiate::parse_request(&event_of(&seen)).expect("a readable request");
    assert_ne!(asked, negotiate::PROTO_VERSION);

    let response = EncodedEvent::new(negotiate::response("NEG-UID", false, NOW));
    server.send(&response).await.expect("write the response");
    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(negotiate::parse_response(&event_of(&seen)), Some(false));

    // Neither side switched, so ordinary traffic still flows as XML.
    let event = EncodedEvent::new(sa("UID-A", "ALPHA"));
    server.send(&event).await.expect("write an event");
    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(seen.mode(), Mode::Xml);
}

#[tokio::test]
async fn messages_are_reassembled_however_the_transport_splits_them() {
    // A one-byte duplex forces every read to land in the middle of something.
    let (mut writer, reader) = tokio::io::duplex(1);
    let mut client = Framed::new(reader, TakCodec::new(Mode::Xml));

    let events: Vec<_> = ["ALPHA", "BRAVO", "CHARLIE"]
        .into_iter()
        .map(|callsign| EncodedEvent::new(sa("UID-A", callsign)))
        .collect();

    let writing = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        for event in &events {
            writer.write_all(event.xml()).await.expect("write");
        }
        writer.flush().await.expect("flush");
    });

    for callsign in ["ALPHA", "BRAVO", "CHARLIE"] {
        let seen = client.next().await.expect("a frame").expect("well framed");
        assert_eq!(event_of(&seen).callsign(), Some(callsign));
    }
    writing.await.expect("the writer finished");
}

#[tokio::test]
async fn a_protobuf_stream_resynchronises_after_corruption() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let mut client = Framed::new(reader, TakCodec::new(Mode::Proto));

    let event = EncodedEvent::new(sa("UID-A", "ALPHA"));
    let writing = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        // Garbage that is not a frame: a magic byte with an unreadable length.
        writer.write_all(&[0xBF]).await.expect("write");
        writer.write_all(&[0xFF; 11]).await.expect("write");
        // Then a well-formed frame, which must still be found.
        let mut framed = BytesMut::new();
        rustak_cot::codec::encode_proto_frame(event.proto(), &mut framed);
        writer.write_all(&framed).await.expect("write");
        writer.flush().await.expect("flush");
    });

    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(event_of(&seen).callsign(), Some("ALPHA"));
    assert!(
        client.codec().skipped() >= 12,
        "the corrupt prefix was counted"
    );
    writing.await.expect("the writer finished");
}

/// The reason `TakCodec::decode` counts a framing failure instead of
/// returning it: `Framed` ends its stream for good on the first decoder error,
/// so reporting an oversized message would disconnect a client over one bad
/// message. The stream has to survive it, and the counters carry the news.
#[tokio::test]
async fn an_oversized_message_is_dropped_without_ending_the_stream() {
    let (mut writer, reader) = tokio::io::duplex(8192);
    let mut client = Framed::new(reader, TakCodec::with_limit(Mode::Xml, 512));

    let writing = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let big = Event::builder("a-u-G", "UID-BIG")
            .how("m-g")
            .point(1.0, 2.0)
            .time(NOW)
            .push(
                rustak_cot::Element::new("remarks").with(rustak_cot::Node::Text("x".repeat(1024))),
            )
            .build();
        writer.write_all(&xml::write(&big)).await.expect("write");
        writer
            .write_all(&xml::write(&sa("UID-A", "ALPHA")))
            .await
            .expect("write");
        writer.flush().await.expect("flush");
    });

    let seen = client.next().await.expect("a frame").expect("well framed");
    assert_eq!(
        event_of(&seen).callsign(),
        Some("ALPHA"),
        "the message after the oversized one still arrives"
    );
    assert_eq!(client.codec().dropped(), 1);
    assert!(client.codec().dropped_bytes() > 1024);
    writing.await.expect("the writer finished");
}

#[tokio::test]
async fn a_peer_that_closes_mid_message_is_not_a_codec_failure() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let mut client = Framed::new(reader, TakCodec::new(Mode::Xml));

    tokio::io::AsyncWriteExt::write_all(&mut writer, b"<event uid=\"A\"><point lat=\"1\"")
        .await
        .expect("write");
    drop(writer);

    assert!(
        client.next().await.is_none(),
        "the stream ends cleanly rather than erroring"
    );
}

#[test]
fn the_decoder_is_usable_without_a_runtime() {
    // `rustak-cot` has no I/O: the codec is a `Decoder`, so a caller that owns
    // its own buffering (a test harness, a replay tool) can use it directly.
    let mut codec = TakCodec::new(Mode::Xml);
    assert_eq!(codec.max_frame(), MAX_MESSAGE);

    let mut buffer = BytesMut::new();
    buffer.extend_from_slice(b"\r\n");
    buffer.extend_from_slice(&xml::write(&sa("UID-A", "ALPHA")));

    let frame = codec
        .decode(&mut buffer)
        .expect("well framed")
        .expect("a whole message");
    assert_eq!(frame.payload().first(), Some(&b'<'));
    assert_eq!(event_of(&frame).callsign(), Some("ALPHA"));
    assert_eq!(codec.decode(&mut buffer).expect("well framed"), None);
}

#[test]
fn a_relayed_frame_keeps_its_bytes_exactly() {
    // The relay path: one encoded event, many recipients, no re-encoding.
    let encoded = EncodedEvent::new(sa("UID-A", "ALPHA"));
    let frame = encoded.frame(Mode::Proto);
    assert_eq!(frame, Frame::Proto(encoded.proto().clone()));

    let mut wire = BytesMut::new();
    rustak_cot::codec::encode_proto_frame(frame.payload(), &mut wire);
    assert_eq!(wire[0], rustak_cot::codec::MAGIC);

    let mut scanner = TakCodec::new(Mode::Proto);
    assert_eq!(
        scanner.decode(&mut wire).expect("well framed"),
        Some(Frame::Proto(encoded.proto().clone()))
    );
}
