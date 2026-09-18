//! A connection's life: replay, the one protocol offer, keepalive, disconnect.
//!
//! The ordering assertions here are the ones that are invisible until a client
//! gets them wrong — a replay that arrives after the protocol offer is a map
//! that fills in the wrong encoding, and a `t-x-takp-r` written after the first
//! protobuf frame is a connection that never recovers.

mod stream_support;

use std::time::Duration;

use rustak_api::identity::Direction;
use rustak_cot::codec::Mode;
use rustak_cot::msgs;
use rustak_cot::types::cot_type;
use rustak_server::pki::RevokeReason;
use rustak_server::prelude::Services as _;

use stream_support::{EXPECT, Harness, SETTLE, settle};

const BOTH: Direction = Direction::Both;

#[tokio::test]
async fn a_new_client_is_given_the_map_before_the_protocol_offer() {
    // `compat/streaming.md` §4: the replay is plain XML and happens *before*
    // any negotiation, because the client has not been offered anything yet.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    alpha.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("ALPHA").await;

    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| {
            config.with_negotiation(false).with_pass_control(true)
        })
        .await;

    let first = bravo
        .expect(|_| true, EXPECT)
        .await
        .expect("something arrives");
    assert_eq!(first.uid, "UID-ALICE", "the map comes first");

    let second = bravo
        .expect(|_| true, EXPECT)
        .await
        .expect("the offer follows");
    assert_eq!(second.r#type, cot_type::TAKP_V);

    harness.stop().await;
}

#[tokio::test]
async fn an_incognito_peer_is_left_out_of_the_replay() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    alpha.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("ALPHA").await;

    alpha
        .send(
            rustak_cot::Event::builder(cot_type::INCOGNITO_ON, "UID-ALICE")
                .point(0.0, 0.0)
                .build(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut bravo = harness.eud(&bob, "BRAVO").await;

    bravo
        .expect_none(SETTLE)
        .await
        .expect("an invisible peer is not replayed to a newcomer");

    harness.stop().await;
}

#[tokio::test]
async fn a_client_that_answers_the_offer_switches_to_protobuf() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;

    // The exchange is driven by reading, and nothing has asked either client to
    // read yet.
    settle(&mut alpha).await;
    settle(&mut bravo).await;

    assert_eq!(alpha.stream().mode(), Mode::Proto);
    assert_eq!(bravo.stream().mode(), Mode::Proto);

    alpha.send_sa(51.5, -0.12).await.unwrap();
    bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("protobuf both ways");
    bravo.send_sa(52.0, -1.0).await.unwrap();
    alpha
        .expect(|event| event.uid == "UID-BOB", EXPECT)
        .await
        .unwrap();
    assert_eq!(
        alpha.stream().server_version(),
        Some(concat!("rustak-", env!("CARGO_PKG_VERSION"))),
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_client_that_never_answers_stays_on_xml_and_is_still_served() {
    // CloudTAK's behaviour: it never sends `t-x-takp-q`, and the offer is
    // harmless to it.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(2).await;

    alpha.send_sa(51.5, -0.12).await.unwrap();
    bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("an XML client is served XML");

    assert_eq!(bravo.stream().mode(), Mode::Xml);

    harness.stop().await;
}

#[tokio::test]
async fn a_protobuf_peer_and_an_xml_peer_still_see_each_other() {
    // The fan-out encodes once per encoding and shares it, so a mixed fleet is
    // the ordinary case rather than a special one.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(2).await;

    settle(&mut alpha).await;
    settle(&mut bravo).await;
    assert_eq!(alpha.stream().mode(), Mode::Proto);
    assert_eq!(bravo.stream().mode(), Mode::Xml);

    alpha.send_sa(51.5, -0.12).await.unwrap();
    bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("the XML peer sees the protobuf peer");

    bravo.send_sa(52.0, -1.0).await.unwrap();
    alpha
        .expect(|event| event.uid == "UID-BOB", EXPECT)
        .await
        .expect("and the other way round");

    harness.stop().await;
}

#[tokio::test]
async fn a_ping_is_answered_only_to_the_client_that_sent_it() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness
        .eud_with(&alice, "ALPHA", |config| {
            config.with_negotiation(false).with_pass_control(true)
        })
        .await;
    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(2).await;

    alpha
        .send(msgs::ping("UID-ALICE", rustak_cot::CotTime::now()))
        .await
        .unwrap();

    let pong = alpha
        .expect(|event| event.r#type == cot_type::PONG, EXPECT)
        .await
        .expect("the pinging client is answered");

    assert_eq!(pong.uid, msgs::PONG_UID, "the uid is the constant takPong");
    assert!(pong.detail.is_empty(), "a pong carries no <detail> at all");

    bravo
        .expect_none(SETTLE)
        .await
        .expect("a keepalive is never relayed");

    harness.stop().await;
}

#[tokio::test]
async fn a_departing_client_is_taken_off_its_peers_maps() {
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

    drop(alpha);

    let notice = bravo
        .expect(|event| event.r#type == cot_type::DISCONNECT, EXPECT)
        .await
        .expect("BRAVO is told that ALPHA has gone");

    let link = notice.detail.find("link").expect("a <link>");
    assert_eq!(link.get("uid"), Some("UID-ALICE"));
    assert_eq!(link.get("type"), Some("a-f-G-U-C"));

    harness.stop().await;
}

#[tokio::test]
async fn a_client_that_never_announced_itself_leaves_without_a_notice() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(2).await;

    drop(alpha);

    bravo
        .expect_none(SETTLE)
        .await
        .expect("nobody ever saw ALPHA arrive, so nothing removes it");

    harness.stop().await;
}

#[tokio::test]
async fn revoking_a_certificate_ends_the_session_it_bought() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    harness.await_connected(1).await;
    alpha.send_sa(51.5, -0.12).await.unwrap();

    harness
        .pki
        .revoke(
            harness.context.db(),
            &alice.fingerprint,
            RevokeReason::AdminAction,
            None,
        )
        .await
        .expect("the certificate is taken back");

    let ended = alpha.expect(|_| true, EXPECT).await;

    assert!(
        ended.is_err(),
        "the connection should end, not carry on: {ended:?}",
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_certificate_from_another_authority_never_reaches_the_application() {
    let harness = Harness::start().await;
    let foreign = rustak_server::pki::testing::TestAuthority::new().await;
    let client = foreign.issue("alice");

    let dir = harness.data_dir.path().join("foreign");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ca.pem"),
        rustak_server::pki::pem_certificate(harness.pki.ca().certificate()),
    )
    .unwrap();
    std::fs::write(
        dir.join("client.pem"),
        rustak_server::pki::pem_certificate(&client.der),
    )
    .unwrap();
    std::fs::write(
        dir.join("client.key"),
        pem::encode(&pem::Pem::new("PRIVATE KEY", client.key_pkcs8.clone())),
    )
    .unwrap();

    let identity = rustak_client::stream::TlsIdentity::from_pem_files(
        dir.join("ca.pem"),
        dir.join("client.pem"),
        dir.join("client.key"),
    )
    .expect("the foreign material loads");

    let config = rustak_client::stream::StreamConfig::new(
        rustak_client::stream::Endpoint::tls(harness.addr.ip().to_string(), harness.addr.port()),
        "UID-FOREIGN",
    )
    .with_tls(identity);

    let mut stream = rustak_client::stream::testing::Eud::connect(&config, "FOREIGN")
        .await
        .expect("the TCP connection is made; the handshake is what fails");

    assert!(
        stream.expect(|_| true, SETTLE).await.is_err(),
        "a certificate from another authority must never become a subscription",
    );
    assert_eq!(harness.live.connected(), 0);

    harness.stop().await;
}

#[tokio::test]
async fn switching_an_account_off_ends_the_session_it_already_had() {
    // R-01 H5. The stream resolves its principal once, at the TLS handshake,
    // and makes no database call thereafter — so disabling an account, which is
    // the control an operator reaches for when a device is lost, used to leave
    // that device receiving every reachable peer's position and injecting CoT
    // until its TCP connection happened to drop. Nothing here revokes a
    // certificate: the disable alone has to end it.
    //
    // `start_with_missions` because it is the harness variant that installs the
    // registry on the context, which is what `sessions::end_all` reaches for —
    // the real runtime installs it unconditionally (`stream::mod`).
    let harness = Harness::start_with_missions().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    harness.await_connected(1).await;
    alpha.send_sa(51.5, -0.12).await.unwrap();

    let user = harness
        .context
        .db()
        .users()
        .get_by_username(&alice.username)
        .await
        .unwrap()
        .expect("the account under test");

    harness
        .context
        .db()
        .users()
        .set_disabled(user.id, true)
        .await
        .expect("the account is switched off");

    let closed = rustak_server::identity::sessions::end_all(&harness.context, &user).await;

    assert_eq!(closed, 1, "the open connection is what had to be closed");

    let ended = alpha.expect(|_| true, EXPECT).await;

    assert!(
        ended.is_err(),
        "a disabled account's connection should end, not carry on: {ended:?}",
    );
    assert_eq!(harness.live.connected(), 0);

    harness.stop().await;
}
