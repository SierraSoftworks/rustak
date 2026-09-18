//! Who hears what: reachability, `<marti><dest>`, flow tags and incognito.
//!
//! Every assertion here is a sentence out of `compat/streaming.md` §8 driven
//! through a real TLS listener with real enrolled clients, because the routing
//! rules are the ones that cannot be got wrong quietly: a mistake does not
//! throw, it shows somebody a position they were not cleared to see.

mod stream_support;

use std::time::Duration;

use rustak_api::identity::Direction;
use rustak_cot::detail::marti::{ALL_STREAMING, Dest, marti_element};
use rustak_cot::detail::{Chat, Element, flow_tags};
use rustak_cot::types::cot_type;
use rustak_server::prelude::Services as _;

use stream_support::{EXPECT, Harness, SETTLE};

/// The two directions a full member of a channel holds.
const BOTH: Direction = Direction::Both;

#[tokio::test]
async fn two_members_of_one_channel_see_each_other() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;

    alpha.send_sa(51.5, -0.12).await.unwrap();

    let seen = bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("BRAVO should see ALPHA");

    assert_eq!(seen.callsign(), Some("ALPHA"));
    assert!((seen.point.lat - 51.5).abs() < 1e-9);

    harness.stop().await;
}

#[tokio::test]
async fn a_client_in_another_channel_hears_nothing() {
    // The rule everything else rests on. `blue` and `green` share no channel,
    // so no message ever crosses between them.
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

    alpha.send_sa(51.5, -0.12).await.unwrap();

    charlie
        .expect_none(SETTLE)
        .await
        .expect("CHARLIE is in another channel");

    harness.stop().await;
}

#[tokio::test]
async fn reachability_is_not_symmetric() {
    // A listener holds `OUT` only: it sees the channel and can say nothing into
    // it. This is the case a naive set intersection gets wrong in both
    // directions at once.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let dora = harness
        .enroll("dora", "UID-DORA", &[("blue", Direction::Out)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut delta = harness.eud(&dora, "DELTA").await;
    harness.await_connected(2).await;

    alpha.send_sa(51.5, -0.12).await.unwrap();
    delta
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("a receive-only member still receives");

    delta.send_sa(52.0, -1.0).await.unwrap();
    alpha
        .expect_none(SETTLE)
        .await
        .expect("a receive-only member publishes to nobody");

    harness.stop().await;
}

#[tokio::test]
async fn a_broadcast_never_comes_back_to_its_sender() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;

    alpha.send_sa(51.5, -0.12).await.unwrap();
    bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .unwrap();

    alpha
        .expect_none(SETTLE)
        .await
        .expect("ALPHA must not be sent its own position");

    harness.stop().await;
}

#[tokio::test]
async fn a_message_addressed_to_a_callsign_reaches_only_that_callsign() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;
    let erin = harness.enroll("erin", "UID-ERIN", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut echo = harness.eud(&erin, "ECHO").await;
    harness.await_connected(3).await;

    // Both recipients have to be known by callsign before anybody addresses one.
    bravo.send_sa(52.0, -1.0).await.unwrap();
    echo.send_sa(53.0, -2.0).await.unwrap();
    harness.await_callsign("BRAVO").await;
    harness.await_callsign("ECHO").await;
    drain(&mut alpha).await;
    drain(&mut bravo).await;
    drain(&mut echo).await;
    drain(&mut bravo).await;
    drain(&mut echo).await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-CHAT-1".to_string();
    chat.detail.push(marti_element(&[Dest::callsign("BRAVO")]));
    alpha.send(chat).await.unwrap();

    let seen = bravo
        .expect(|event| event.uid == "UID-CHAT-1", EXPECT)
        .await
        .expect("BRAVO was addressed");
    assert!(
        seen.detail.find("marti").is_none(),
        "<marti> is stripped from every relay",
    );

    echo.expect_none(SETTLE)
        .await
        .expect("ECHO was not addressed");

    harness.stop().await;
}

#[tokio::test]
async fn all_streaming_discards_the_callsign_list_and_broadcasts() {
    // ATAK's "post to all" on a client that also had somebody selected.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;
    let erin = harness.enroll("erin", "UID-ERIN", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut echo = harness.eud(&erin, "ECHO").await;
    harness.await_connected(3).await;

    bravo.send_sa(52.0, -1.0).await.unwrap();
    echo.send_sa(53.0, -2.0).await.unwrap();
    harness.await_callsign("ECHO").await;
    drain(&mut alpha).await;
    drain(&mut bravo).await;
    drain(&mut echo).await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-CHAT-2".to_string();
    chat.detail.push(marti_element(&[
        Dest::callsign("BRAVO"),
        Dest::callsign(ALL_STREAMING),
    ]));
    alpha.send(chat).await.unwrap();

    bravo
        .expect(|event| event.uid == "UID-CHAT-2", EXPECT)
        .await
        .unwrap();
    echo.expect(|event| event.uid == "UID-CHAT-2", EXPECT)
        .await
        .expect("'All Streaming' reaches everybody the sender can reach");

    harness.stop().await;
}

#[tokio::test]
async fn a_message_addressed_to_a_uid_reaches_that_device() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;
    let erin = harness.enroll("erin", "UID-ERIN", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut echo = harness.eud(&erin, "ECHO").await;
    harness.await_connected(3).await;

    bravo.send_sa(52.0, -1.0).await.unwrap();
    echo.send_sa(53.0, -2.0).await.unwrap();
    harness.await_callsign("ECHO").await;
    drain(&mut alpha).await;
    drain(&mut bravo).await;
    drain(&mut echo).await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-CHAT-3".to_string();
    chat.detail.push(marti_element(&[Dest::uid("UID-BOB")]));
    alpha.send(chat).await.unwrap();

    bravo
        .expect(|event| event.uid == "UID-CHAT-3", EXPECT)
        .await
        .unwrap();
    echo.expect_none(SETTLE).await.unwrap();

    harness.stop().await;
}

#[tokio::test]
async fn addressing_a_callsign_in_another_channel_reaches_nobody() {
    // The gotcha: a `<dest>` narrows who receives a message, it never widens
    // who may be reached.
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

    charlie.send_sa(53.0, -2.0).await.unwrap();
    harness.await_callsign("CHARLIE").await;

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-CHAT-4".to_string();
    chat.detail
        .push(marti_element(&[Dest::callsign("CHARLIE")]));
    alpha.send(chat).await.unwrap();

    charlie
        .expect_none(SETTLE)
        .await
        .expect("naming a callsign is not a way around the channels");

    harness.stop().await;
}

#[tokio::test]
async fn a_relayed_message_carries_this_servers_flow_tag() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;

    alpha.send_sa(51.5, -0.12).await.unwrap();

    let seen = bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .unwrap();

    let server_id = harness.context.config().server.name.clone();
    assert!(
        flow_tags::has_flow_tag(&seen.detail, &server_id),
        "a relay is stamped so that a bridged server can tell it has been here",
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_message_that_has_already_been_here_is_dropped() {
    // Loop suppression between two bridged servers, which is the only reason
    // the flow tag exists.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;

    let server_id = harness.context.config().server.name.clone();
    let mut looped = alpha.sa(51.5, -0.12);
    looped.uid = "UID-LOOPED".to_string();
    looped.detail.push(Element::new(flow_tags::ELEMENT).attr(
        flow_tags::flow_tag_name(&server_id),
        "2026-09-18T00:00:00.000Z",
    ));
    alpha.send(looped).await.unwrap();

    bravo
        .expect_none(SETTLE)
        .await
        .expect("a message carrying our own tag has been here before");

    harness.stop().await;
}

#[tokio::test]
async fn an_incognito_client_reaches_only_the_people_it_names() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;
    let erin = harness.enroll("erin", "UID-ERIN", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut echo = harness.eud(&erin, "ECHO").await;
    harness.await_connected(3).await;

    bravo.send_sa(52.0, -1.0).await.unwrap();
    echo.send_sa(53.0, -2.0).await.unwrap();
    harness.await_callsign("ECHO").await;
    drain(&mut alpha).await;
    drain(&mut bravo).await;
    drain(&mut echo).await;

    alpha
        .send(
            rustak_cot::Event::builder(cot_type::INCOGNITO_ON, "UID-ALICE")
                .point(0.0, 0.0)
                .build(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    alpha.send_sa(51.5, -0.12).await.unwrap();
    bravo
        .expect_none(SETTLE)
        .await
        .expect("an incognito client broadcasts to nobody");

    let mut chat = alpha.sa(51.5, -0.12);
    chat.r#type = cot_type::CHAT.to_string();
    chat.uid = "UID-CHAT-5".to_string();
    chat.detail.push(marti_element(&[Dest::callsign("BRAVO")]));
    alpha.send(chat).await.unwrap();

    bravo
        .expect(|event| event.uid == "UID-CHAT-5", EXPECT)
        .await
        .expect("an incognito client still reaches who it names");
    echo.expect_none(SETTLE).await.unwrap();

    harness.stop().await;
}

#[tokio::test]
async fn an_undeliverable_direct_chat_comes_back_to_its_sender() {
    // The half of `chat-direct` that a server has to implement: a chat typed at
    // a named person who is not there must not disappear, because the client
    // shows an unanswered message as sent.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;

    bravo.send_sa(52.0, -1.0).await.unwrap();
    harness.await_callsign("BRAVO").await;
    drain(&mut alpha).await;
    drain(&mut bravo).await;

    // A callsign the server knows: delivered, and the sender hears nothing.
    let mut delivered = alpha.sa(51.5, -0.12);
    delivered.r#type = cot_type::CHAT.to_string();
    delivered.uid = "UID-CHAT-6".to_string();
    delivered.detail.push(chat_element("UID-BOB"));
    delivered
        .detail
        .push(marti_element(&[Dest::callsign("BRAVO")]));
    alpha.send(delivered).await.unwrap();

    bravo
        .expect(|event| event.uid == "UID-CHAT-6", EXPECT)
        .await
        .expect("BRAVO was addressed by a callsign the server knows");
    alpha
        .expect_none(SETTLE)
        .await
        .expect("a delivered chat owes its sender nothing");

    // A callsign nobody answers to: the sender gets its own message back.
    let mut undeliverable = alpha.sa(51.5, -0.12);
    undeliverable.r#type = cot_type::CHAT.to_string();
    undeliverable.uid = "UID-CHAT-7".to_string();
    undeliverable.detail.push(chat_element("UID-GHOST"));
    undeliverable
        .detail
        .push(marti_element(&[Dest::callsign("GHOST")]));
    alpha.send(undeliverable).await.unwrap();

    let bounce = alpha
        .expect(|event| event.r#type == cot_type::CHAT_FAILED, EXPECT)
        .await
        .expect("an undeliverable chat bounces");

    assert_eq!(bounce.uid, "UID-CHAT-7", "the sender's own message back");
    assert_eq!(
        bounce.detail.get::<Chat>().and_then(|chat| chat.id),
        Some("UID-GHOST".to_string()),
        "carrying the conversation the client files it under",
    );
    assert!(
        bounce.detail.find("marti").is_none(),
        "the address list never survives, not even on the way back",
    );

    let server_id = harness.context.config().server.name.clone();
    assert!(
        !flow_tags::has_flow_tag(&bounce.detail, &server_id),
        "a bounce is not a relay and carries no tag of ours",
    );

    bravo
        .expect_none(SETTLE)
        .await
        .expect("a bounce is for the sender alone");

    harness.stop().await;
}

/// The `<__chat>` a direct message carries, naming the conversation.
fn chat_element(conversation: &str) -> Element {
    Element::new("__chat")
        .attr("id", conversation)
        .attr("chatroom", conversation)
        .attr("senderCallsign", "ALPHA")
        .attr("messageId", "MSG-0001")
}

/// Reads whatever a client has already been sent, so that a later assertion is
/// about what happens next rather than about the replay.
async fn drain(eud: &mut rustak_client::stream::testing::Eud) {
    let _ = eud.expect_none(Duration::from_millis(150)).await;
}
