//! The stream client driven the way a server drives it.
//!
//! The unit tests in `stream/` cover the pieces in isolation — the connect
//! string, the negotiation transitions, the keepalive arithmetic. These cover
//! the thing those pieces exist to produce: a client on one end of a socket
//! and a server's framing on the other, with the negotiation, the mode switch,
//! the ping and the death clock all happening inside `poll_next` the way they
//! do in production.
//!
//! The "server" here is a `Framed<_, TakCodec>` over `tokio::io::duplex`, which
//! is the same codec `rustak-server` uses — so a change that breaks these
//! breaks the real pairing too.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rustak_client::stream::{Mode, Negotiation, StreamError, TakStream};
use rustak_cot::codec::{EncodedEvent, Frame, Mode as CotMode, TakCodec};
use rustak_cot::detail::{Contact, Group};
use rustak_cot::types::cot_type;
use rustak_cot::{CotTime, Event, msgs, negotiate, proto, xml};
use tokio::io::DuplexStream;
use tokio_util::codec::Framed;

/// The instant the golden fixtures use, so every event here is reproducible.
const NOW: CotTime = CotTime::from_millis(1_789_646_400_000);

/// The other end of the socket: what `rustak-server`'s connection task is.
type Server = Framed<DuplexStream, TakCodec>;

fn pair() -> (Server, TakStream) {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);

    (
        Framed::new(server_io, TakCodec::new(CotMode::Xml)),
        TakStream::over(client_io, "ANDROID-ALPHA"),
    )
}

fn sa(uid: &str, callsign: &str) -> Event {
    Event::builder("a-f-G-U-C", uid)
        .how("m-g")
        .point(51.5074, -0.1278)
        .time(NOW)
        .stale_after(Duration::from_secs(120))
        .typed(&Contact::new(callsign).with_endpoint("*:-1:stcp"))
        .typed(&Group::new("Cyan", "Team Member"))
        .build()
}

async fn send(server: &mut Server, event: Event) {
    server
        .send(&EncodedEvent::new(event))
        .await
        .expect("the server can write");
}

/// Lets the client run its own protocol for a moment.
///
/// A `Stream` only acts when it is polled: the negotiation request, the mode
/// switch and the keepalive ping all happen inside `poll_next`. A sidecar's
/// `while let Some(event) = stream.next().await` supplies that for free; a test
/// that wants to look at the socket in between has to say so.
async fn settle(client: &mut TakStream) {
    let _ = tokio::time::timeout(Duration::from_millis(50), client.next()).await;
}

async fn next(server: &mut Server) -> Event {
    let frame = server
        .next()
        .await
        .expect("the client is still connected")
        .expect("the client's bytes frame");

    match &frame {
        Frame::Xml(bytes) => xml::parse(bytes).expect("the client writes parseable XML"),
        Frame::Proto(payload) => proto::message_to_event(
            proto::decode(payload).expect("the client writes decodable protobuf"),
        )
        .expect("the payload carries a cotEvent"),
    }
}

#[tokio::test]
async fn a_client_answers_the_offer_and_switches_both_directions_to_protobuf() {
    // The whole `compat/streaming.md` §4 exchange, from the client's side: the
    // replay arrives as XML, the offer is answered with a request reusing its
    // uid, and the response flips the encoding for everything after it.
    let (mut server, mut client) = pair();

    send(&mut server, sa("ANDROID-BRAVO", "BRAVO")).await;
    send(
        &mut server,
        negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW),
    )
    .await;

    let replayed = client
        .next()
        .await
        .expect("the replay is delivered")
        .expect("and it parses");
    assert_eq!(replayed.uid, "ANDROID-BRAVO");

    // The offer is consumed rather than delivered, and answered.
    settle(&mut client).await;
    let request = next(&mut server).await;
    assert_eq!(request.r#type, cot_type::TAKP_Q);
    assert_eq!(request.uid, "NEG-1", "the request reuses the offer's uid");
    assert_eq!(negotiate::parse_request(&request), Some(1));

    send(&mut server, negotiate::response("NEG-1", true, NOW)).await;
    server.codec_mut().set_mode(CotMode::Proto);

    // Both directions are protobuf from here: the client reads one…
    send(&mut server, sa("ANDROID-CHARLIE", "CHARLIE")).await;
    let after = client
        .next()
        .await
        .expect("a protobuf message is delivered")
        .expect("and it decodes");

    assert_eq!(after.uid, "ANDROID-CHARLIE");
    assert_eq!(client.mode(), Mode::Proto);
    assert_eq!(client.negotiation(), Negotiation::Proto);
    assert_eq!(client.server_version(), Some("rustak-0.1.0"));

    // …and writes one.
    client.send(sa("ANDROID-ALPHA", "ALPHA")).await.unwrap();
    let ours = next(&mut server).await;
    assert_eq!(ours.callsign(), Some("ALPHA"));
}

#[tokio::test]
async fn events_written_during_negotiation_are_held_back_until_it_settles() {
    // The hazard this exists for: an event written between our request and the
    // server's response goes out in XML to a server that has already switched
    // its reader to protobuf.
    let (mut server, mut client) = pair();

    send(
        &mut server,
        negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW),
    )
    .await;

    // Drive the client until it has asked. Nothing is delivered, because an
    // offer is control traffic.
    tokio::time::timeout(Duration::from_millis(100), client.next())
        .await
        .expect_err("an offer is not delivered to the caller");

    let request = next(&mut server).await;
    assert_eq!(request.r#type, cot_type::TAKP_Q);
    assert_eq!(client.negotiation(), Negotiation::Requested);

    client.send(sa("ANDROID-ALPHA", "ALPHA")).await.unwrap();
    assert_eq!(client.queued(), 1, "the event is held, not written");

    tokio::time::timeout(Duration::from_millis(100), server.next())
        .await
        .expect_err("nothing may be written while the request is outstanding");

    send(&mut server, negotiate::response("NEG-1", true, NOW)).await;
    server.codec_mut().set_mode(CotMode::Proto);

    tokio::time::timeout(Duration::from_millis(100), client.next())
        .await
        .expect_err("the response is not delivered either");

    // Released, and released in the encoding that was decided rather than the
    // one it was written in.
    let held = next(&mut server).await;
    assert_eq!(held.callsign(), Some("ALPHA"));
    assert_eq!(client.queued(), 0);
}

#[tokio::test]
async fn a_refusal_leaves_both_ends_on_xml_and_releases_what_was_held() {
    let (mut server, mut client) = pair();

    send(
        &mut server,
        negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW),
    )
    .await;
    settle(&mut client).await;
    next(&mut server).await;

    client.send(sa("ANDROID-ALPHA", "ALPHA")).await.unwrap();
    send(&mut server, negotiate::response("NEG-1", false, NOW)).await;

    let _ = tokio::time::timeout(Duration::from_millis(100), client.next()).await;

    let held = next(&mut server).await;
    assert_eq!(held.callsign(), Some("ALPHA"));
    assert_eq!(client.mode(), Mode::Xml);
    assert_eq!(client.negotiation(), Negotiation::Xml);
}

#[tokio::test]
async fn a_server_that_never_offers_is_a_connection_that_stays_on_xml() {
    // CloudTAK's behaviour, and that of any server that does not implement the
    // exchange. Nothing is buffered, because nothing was ever asked.
    let (mut server, mut client) = pair();

    send(&mut server, sa("ANDROID-BRAVO", "BRAVO")).await;
    client.send(sa("ANDROID-ALPHA", "ALPHA")).await.unwrap();

    assert_eq!(next(&mut server).await.callsign(), Some("ALPHA"));
    assert_eq!(client.mode(), Mode::Xml);
    assert_eq!(client.negotiation(), Negotiation::Waiting);
    assert_eq!(client.queued(), 0);
}

#[tokio::test(start_paused = true)]
async fn sixty_seconds_without_an_answer_settles_on_xml_rather_than_hanging() {
    // `research/07` §3.4: the silent fallback is the correct one. The server
    // here keeps the connection alive with ordinary traffic every 15 seconds,
    // so that the negotiation deadline — not the 25-second keepalive — is what
    // fires.
    let (mut server, mut client) = pair();

    send(
        &mut server,
        negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW),
    )
    .await;
    settle(&mut client).await;
    next(&mut server).await;

    client.send(sa("ANDROID-ALPHA", "ALPHA")).await.unwrap();
    assert_eq!(client.queued(), 1);

    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(15)).await;
        send(&mut server, sa("ANDROID-BRAVO", "BRAVO")).await;

        let delivered = tokio::time::timeout(Duration::from_secs(2), client.next()).await;
        assert!(delivered.is_ok(), "the connection is alive throughout");
    }

    assert_eq!(client.negotiation(), Negotiation::Xml);
    assert_eq!(client.mode(), Mode::Xml);
    assert_eq!(client.queued(), 0, "the held event was released");
    assert_eq!(next(&mut server).await.callsign(), Some("ALPHA"));
}

#[tokio::test(start_paused = true)]
async fn a_quiet_connection_is_pinged_on_ataks_clock_and_then_declared_dead() {
    // 15 s, 4.5 s, 25 s — `compat/streaming.md` §6. The ping's shape matters as
    // much as its timing: `rustak-server` answers `t-x-c-t` and nothing else.
    let (mut server, mut client) = pair();

    let dead = tokio::spawn(async move {
        let outcome = client.next().await;

        assert!(
            matches!(outcome, Some(Err(StreamError::RxTimeout))),
            "a silent server is a dead connection",
        );
    });

    let first = next(&mut server).await;
    assert!(msgs::is_ping(&first), "{first:?}");
    assert_eq!(first.uid, "ANDROID-ALPHA-ping");
    assert_eq!(first.how.as_deref(), Some("m-g"));
    assert_eq!(
        first.stale - first.time,
        10_000,
        "the ping is stale in 10 s"
    );

    let second = next(&mut server).await;
    assert!(msgs::is_ping(&second));

    dead.await.expect("the client gives up");
}

#[tokio::test(start_paused = true)]
async fn a_pong_keeps_the_connection_alive_without_reaching_the_caller() {
    // The server's answer is consumed: a plugin has no use for it, and a
    // `while let Some(event)` loop that saw one would have to filter it. The
    // client runs in a task of its own here because that is what keeps it
    // polled — which is what a sidecar's own loop does.
    let (mut server, mut client) = pair();
    let (delivered, mut caller) = tokio::sync::mpsc::unbounded_channel();

    let driver = tokio::spawn(async move {
        while let Some(event) = client.next().await {
            if delivered.send(event.map(|event| event.r#type)).is_err() {
                break;
            }
        }
    });

    for _ in 0..4 {
        let ping = tokio::time::timeout(Duration::from_secs(20), next(&mut server))
            .await
            .expect("the client pings a silent server");
        assert!(msgs::is_ping(&ping), "{ping:?}");

        send(&mut server, msgs::pong(CotTime::now())).await;
    }

    drop(server);
    driver
        .await
        .expect("the client task ends when the server hangs up");

    assert!(
        caller.recv().await.is_none(),
        "a pong is not an event the caller sees",
    );
}

#[tokio::test(start_paused = true)]
async fn pass_control_delivers_the_control_traffic_as_well_as_acting_on_it() {
    // What a conformance test needs: the negotiation and the pong visible,
    // without giving up the client's own handling of them.
    let (mut server, mut client) = pair();
    client.set_pass_control(true);

    send(
        &mut server,
        negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW),
    )
    .await;

    let offer = client.next().await.unwrap().unwrap();
    assert_eq!(offer.r#type, cot_type::TAKP_V);

    let request = next(&mut server).await;
    assert_eq!(request.r#type, cot_type::TAKP_Q);

    send(&mut server, negotiate::response("NEG-1", true, NOW)).await;
    let response = client.next().await.unwrap().unwrap();

    assert_eq!(response.r#type, cot_type::TAKP_R);
    assert_eq!(client.mode(), Mode::Proto, "it still acted on it");
}

#[tokio::test]
async fn one_unreadable_message_is_counted_rather_than_ending_the_connection() {
    // `M1-03`: the codec never returns a stream-ending error, and the client
    // must not invent one. An `<event>` with no `<point>` is the realistic
    // case — TAK Server drops those too.
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut client = TakStream::over(client_io, "ANDROID-ALPHA");
    let mut raw = server_io;

    use tokio::io::AsyncWriteExt;
    raw.write_all(b"<event uid=\"NO-POINT\" type=\"a-f-G\"></event>")
        .await
        .unwrap();
    raw.write_all(&xml::write(&sa("ANDROID-BRAVO", "BRAVO")))
        .await
        .unwrap();

    let delivered = client.next().await.unwrap().unwrap();

    assert_eq!(delivered.uid, "ANDROID-BRAVO");
    assert_eq!(client.dropped(), 1);
}

#[tokio::test]
async fn a_server_that_hangs_up_ends_the_stream_rather_than_erroring() {
    let (server, mut client) = pair();
    drop(server);

    assert!(client.next().await.is_none());
    assert!(client.next().await.is_none(), "and stays ended");
}

#[cfg(feature = "testing")]
#[tokio::test(start_paused = true)]
async fn two_euds_wired_together_behave_like_two_clients() {
    // The shape `rustak-server`'s integration tests are written in, checked
    // without a server: send from one, assert on the other, assert the silence
    // on a third.
    use rustak_client::stream::testing::Eud;

    let (left, right) = tokio::io::duplex(8192);
    let mut alpha = Eud::over(left, "ANDROID-ALPHA", "ALPHA");
    let mut bravo = Eud::over(right, "ANDROID-BRAVO", "BRAVO").with_team("Blue", "Team Lead");

    bravo.send_sa(51.5, -0.12).await.unwrap();

    let seen = alpha
        .expect(
            |event| event.callsign() == Some("BRAVO"),
            Duration::from_secs(1),
        )
        .await
        .expect("BRAVO's position arrives");

    assert_eq!(seen.uid, "ANDROID-BRAVO");
    assert_eq!(seen.group().unwrap().name, "Blue");

    bravo
        .expect_none(Duration::from_millis(100))
        .await
        .expect("ALPHA has sent nothing");
}

#[tokio::test]
async fn settle_finishes_the_protocol_work_and_keeps_what_arrived_meanwhile() {
    // The trap `TakStream::settle` exists for: a caller that sends and then
    // waits for the effect on somebody *else* never polls the connection, so
    // the held event would sit there forever. Settling must also not cost the
    // events that arrive while it runs.
    let (mut server, mut client) = pair();

    send(
        &mut server,
        negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW),
    )
    .await;
    settle(&mut client).await;
    next(&mut server).await;
    assert_eq!(client.negotiation(), Negotiation::Requested);

    client.send(sa("ANDROID-ALPHA", "ALPHA")).await.unwrap();
    assert_eq!(client.queued(), 1, "held until the encoding is decided");

    // The server answers and, in the same breath, relays a peer's position.
    send(&mut server, negotiate::response("NEG-1", true, NOW)).await;
    server.codec_mut().set_mode(CotMode::Proto);
    send(&mut server, sa("ANDROID-BRAVO", "BRAVO")).await;

    client
        .settle(Duration::from_millis(100))
        .await
        .expect("settling does not fail");

    assert_eq!(client.queued(), 0);
    assert_eq!(client.mode(), Mode::Proto);
    assert_eq!(next(&mut server).await.callsign(), Some("ALPHA"));

    let held = client
        .next()
        .await
        .expect("the peer's position was kept")
        .expect("and it parses");
    assert_eq!(held.uid, "ANDROID-BRAVO");
}
