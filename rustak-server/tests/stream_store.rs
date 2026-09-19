//! What the stream leaves behind, and the two cases that only show up on a
//! real socket: a channel-addressed message and an oversize protobuf frame.

mod stream_support;

use std::time::Duration;

use rustak_api::identity::Direction;
use rustak_cot::codec::Mode;
use rustak_cot::detail::marti::{Dest, marti_element};
use rustak_cot::detail::{Element, Node, fileshare::FileShare};
use rustak_cot::types::cot_type;
use rustak_server::cot_store;
use rustak_server::prelude::Services as _;

use stream_support::{EXPECT, Harness, SETTLE, settle};

const BOTH: Direction = Direction::Both;

#[tokio::test]
async fn a_relayed_message_is_readable_back_out_of_the_latest_store() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    harness.await_connected(1).await;
    alpha.send_sa(51.5, -0.12).await.unwrap();

    let stored = await_stored(&harness, "UID-ALICE").await;

    assert!(
        stored.contains("_flow-tags_"),
        "what is stored is what was relayed, tag and all: {stored}",
    );
    assert!(
        !stored.contains("<marti"),
        "and <marti> is never part of it: {stored}",
    );
    assert!(stored.contains("ALPHA"));

    harness.stop().await;
}

#[tokio::test]
async fn the_history_segments_hold_every_message_a_device_sent() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    harness.await_connected(1).await;
    alpha.send_sa(51.5, -0.12).await.unwrap();
    await_stored(&harness, "UID-ALICE").await;

    let segments = harness
        .context
        .db()
        .stream_segments()
        .overlapping(
            cot_store::STREAM_KIND,
            "UID-ALICE",
            chrono::Utc::now() - chrono::Duration::hours(1),
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();

    assert_eq!(segments.len(), 1, "one open segment for the one device");

    harness.stop().await;
}

#[tokio::test]
async fn a_keepalive_is_never_recorded() {
    // History is what happened in the world; a ping is a fact about a socket.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness
        .eud_with(&alice, "ALPHA", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(1).await;

    alpha
        .send(rustak_cot::msgs::ping(
            "UID-ALICE",
            rustak_cot::CotTime::now(),
        ))
        .await
        .unwrap();
    settle(&mut alpha).await;

    assert!(
        cot_store::latest_xml(harness.context.db(), "UID-ALICE-ping")
            .await
            .unwrap()
            .is_none(),
        "a keepalive is consumed, not relayed and not stored",
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_channel_addressed_message_reaches_that_channels_readers() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH), ("green", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;
    let carol = harness
        .enroll("carol", "UID-CAROL", &[("green", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut charlie = harness.eud(&carol, "CHARLIE").await;
    harness.await_connected(3).await;
    settle(&mut alpha).await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-GREEN-1".to_string();
    chat.detail.push(marti_element(&[Dest::group("green")]));
    alpha.send(chat).await.unwrap();

    charlie
        .expect(|event| event.uid == "UID-GREEN-1", EXPECT)
        .await
        .expect("a reader of the named channel");
    bravo
        .expect_none(SETTLE)
        .await
        .expect("a reader of a different channel");

    harness.stop().await;
}

#[tokio::test]
async fn a_channel_the_sender_may_not_publish_into_reaches_nobody() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let carol = harness
        .enroll("carol", "UID-CAROL", &[("green", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut charlie = harness.eud(&carol, "CHARLIE").await;
    harness.await_connected(2).await;
    settle(&mut alpha).await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-GREEN-2".to_string();
    chat.detail.push(marti_element(&[Dest::group("green")]));
    alpha.send(chat).await.unwrap();

    charlie
        .expect_none(SETTLE)
        .await
        .expect("re-addressing to a channel needs the IN grant on it");

    harness.stop().await;
}

#[tokio::test]
async fn a_message_addressed_to_a_mission_is_accepted_and_reaches_nobody() {
    // The M1 stub. Accepted rather than refused, because a client addressing a
    // Data Sync this server does not have yet must not lose its connection.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;
    settle(&mut alpha).await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-MISSION-1".to_string();
    chat.detail
        .push(marti_element(&[Dest::mission("Operation Kettle")]));
    alpha.send(chat).await.unwrap();

    bravo.expect_none(SETTLE).await.unwrap();

    // The connection is still good, which is the point.
    alpha.send_sa(51.6, -0.13).await.unwrap();
    bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("the connection carried on");

    harness.stop().await;
}

#[tokio::test]
async fn an_oversize_message_reaches_a_protobuf_peer_as_a_pointer() {
    // ATAK's receive buffer is 64 KiB and it resynchronises rather than growing
    // one, so a frame past that is a frame it would never read.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness
        .eud_with(&alice, "ALPHA", |config| config.with_negotiation(false))
        .await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;
    settle(&mut bravo).await;
    assert_eq!(bravo.stream().mode(), Mode::Proto);

    let mut huge = alpha.sa(51.5, -0.12);
    huge.r#type = cot_type::CHAT.to_string();
    huge.uid = "UID-HUGE".to_string();
    huge.detail
        .push(Element::new("remarks").with(Node::Text("x".repeat(70_000))));
    alpha.send(huge).await.unwrap();

    let pointer = bravo
        .expect(|event| event.r#type == cot_type::FILESHARE, EXPECT)
        .await
        .expect("a pointer stands in for the oversize message");

    let share = pointer.detail.get::<FileShare>().expect("a <fileshare>");
    assert_eq!(
        share.sender_url,
        "https://localhost:8446/Marti/api/cot/xml/UID-HUGE",
    );
    assert_eq!(share.sha256.len(), 64);
    assert_eq!(
        share.sender_callsign, "ALPHA",
        "an empty senderCallsign is what the receiver would display",
    );

    // The **server-generated** template, not ATAK's own offer. Research 05 §9
    // pins two different `b-f-t-r` shapes and `compat/files.md` §9 used to give
    // only the client's; ten seconds is the whole window ATAK has to notice the
    // pointer and fetch, and an `<ackrequest>` the server will not consume only
    // makes it send a `b-f-t-a` into the void. R-02 M1.
    assert_eq!(
        pointer.stale,
        pointer.time.stale_after(Duration::from_secs(100)),
        "the substitute is valid for 100s",
    );
    assert!(
        pointer.detail.find("ackrequest").is_none(),
        "the server-generated template carries no <ackrequest>: {:?}",
        pointer.detail,
    );
    assert_eq!(
        pointer.point.hae, 9_999_999.0,
        "hae=0.0 would plot the pointer at sea level rather than at unknown altitude",
    );

    // And the message itself is readable at the address it points to. Until
    // R-02 H1 that URL matched no route at all, so the substitution lost the
    // message and showed the user a failed transfer.
    let stored = await_stored(&harness, "UID-HUGE").await;
    assert!(stored.len() > 70_000);

    let fetched = fetch(&harness, &alice, &share.sender_url).await;

    assert_eq!(
        fetched.0, 200,
        "the pointer's own URL: {}",
        share.sender_url
    );
    assert!(
        fetched.1.contains("uid=\"UID-HUGE\""),
        "and it answers the original event: {}",
        &fetched.1[..fetched.1.len().min(200)],
    );

    harness.stop().await;
}

/// `GET`s a `senderUrl` against this harness's own Marti routes.
///
/// The pointer names an absolute URL built from `[marti] public_host`; what is
/// asserted here is that its **path** is one the server serves, which is the
/// half the substitution got wrong.
async fn fetch(
    harness: &Harness,
    identity: &stream_support::Identity,
    sender_url: &str,
) -> (u16, String) {
    use actix_web::{App, test, web};

    let path = sender_url
        .split_once("/Marti/")
        .map(|(_, tail)| format!("/Marti/{tail}"))
        .expect("a senderUrl under /Marti");

    let issuer = rustak_server::auth::JwtIssuer::load_or_adopt(
        harness.context.db(),
        harness.context.secrets(),
        &harness.context.config().auth,
        "https://localhost:8446",
        &rustak_server::testing::keys::JWT_SIGNING_KEY,
    )
    .await
    .expect("the issuer");
    let user = harness
        .context
        .db()
        .users()
        .get_by_username(&identity.username)
        .await
        .unwrap()
        .expect("the account under test");
    let session = rustak_server::testing::session_for(&harness.context, &user, false).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(harness.context.clone()))
            .app_data(web::Data::new(std::sync::Arc::new(issuer)))
            .configure(rustak_server::marti::services(
                rustak_server::marti::ListenerRole::Public,
            )),
    )
    .await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&path)
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request(),
    )
    .await;

    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&test::read_body(response).await).into_owned();

    (status, body)
}

#[tokio::test]
async fn an_oversize_message_reaches_an_xml_peer_whole() {
    // The 64 KiB ceiling is a protobuf frame limit; substituting for an XML
    // client would lose a message that would have arrived intact.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness
        .eud_with(&alice, "ALPHA", |config| config.with_negotiation(false))
        .await;
    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(2).await;

    let mut huge = alpha.sa(51.5, -0.12);
    huge.r#type = cot_type::CHAT.to_string();
    huge.uid = "UID-HUGE-XML".to_string();
    huge.detail
        .push(Element::new("remarks").with(Node::Text("x".repeat(70_000))));
    alpha.send(huge).await.unwrap();

    let seen = bravo
        .expect(|event| event.uid == "UID-HUGE-XML", EXPECT)
        .await
        .expect("an XML client gets the whole message");

    assert_eq!(seen.r#type, cot_type::CHAT);

    harness.stop().await;
}

/// Waits for the store's writer task to have committed a uid, or gives up.
#[tokio::test]
async fn what_the_stream_relayed_is_written_before_the_server_finishes_stopping() {
    // R-03 M4. The store used to stop on a *child* of the server's own
    // shutdown, so on `SIGTERM` its writer broke its loop and closed its
    // segments while the listener was still draining connections — against a
    // module whose whole invariant is that what it holds has already happened.
    // It now has its own token, cancelled only once `listener_tls::run` has
    // returned and nothing can relay anything else.
    //
    // Deliberately no `await_stored` before the stop: "stopping flushes what
    // was relayed" is the assertion, so waiting for it first would assert
    // nothing.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    harness.await_connected(1).await;
    alpha.send_sa(51.5, -0.12).await.unwrap();

    // Pumped, so the message has certainly been read and routed — and so
    // `Router::record` has certainly queued it — before anything stops.
    settle(&mut alpha).await;

    // Cloned, because stopping consumes the harness and the database outlives
    // the listener that wrote to it.
    let context = harness.context.clone();
    harness.stop().await;

    let stored = cot_store::latest_xml(context.db(), "UID-ALICE")
        .await
        .expect("the store is readable after the listener has gone");

    assert!(
        stored.is_some_and(|xml| xml.contains("ALPHA")),
        "a message the stream relayed is missing from the record",
    );
}

async fn await_stored(harness: &Harness, uid: &str) -> String {
    for _ in 0..200 {
        if let Some(xml) = cot_store::latest_xml(harness.context.db(), uid)
            .await
            .unwrap()
        {
            return xml;
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("{uid} was never recorded");
}
