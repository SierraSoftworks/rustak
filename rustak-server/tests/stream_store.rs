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

    // And the message itself is still readable at the address it points to.
    let stored = await_stored(&harness, "UID-HUGE").await;
    assert!(stored.len() > 70_000);

    harness.stop().await;
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
