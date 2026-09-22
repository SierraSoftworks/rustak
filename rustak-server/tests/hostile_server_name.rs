//! The 2026-09-22 outage, written down as a test that would have caught it.
//!
//! `[server] name` is free text an operator types into a configuration file,
//! and the flow tag rustak stamps on every relayed message is an XML attribute
//! **named** after it. This installation's was `SierraSoftworks TAK`, so every
//! message rustak relayed for three days went out as
//! `<_flow-tags_ TAK-Server-SierraSoftworks TAK="…">`. CloudTAK's sax parser
//! read that as an attribute with no value and dropped each message off its
//! socket; the mission `/cot` document, which is parsed whole, answered `500`
//! for any mission holding a single item. rustak logged nothing, because rustak
//! had answered `200`.
//!
//! `9c6f6f4` fixed it three ways over. What it could not fix is that **no suite
//! would have noticed**: every harness and interop configuration called the
//! server `rustak`, `rustak-1` or `rustak-interop`, one-word names that happen
//! to be legal as XML names. So the ordinary test installation is now called
//! [`TEST_SERVER_NAME`] — `Rustak Test & Co. (näme)` — and this file is the
//! explicit statement of what that has to buy:
//!
//! 1. A relay reaches a **protobuf** peer and an **XML** peer alike, and what
//!    the XML peer was sent is markup a parser that owes us nothing will read.
//! 2. The two `<events>` documents built from stored rows — the uid history at
//!    `/Marti/api/cot/xml/{uid}/all` and a mission's `/cot` — are too.
//!
//! The strict reader is `quick-xml` with attribute checking on, which refuses
//! the production document for the same reason sax did, plus the XML 1.0
//! `Name` production from `rustak_cot::xml`. Both halves matter: quick-xml
//! catches `name value="x"`, and `is_name` catches a name that is well-formed
//! to read but illegal to write, such as one starting with a digit.
//!
//! Run with `cargo test -p rustak-server --features testing --test hostile_server_name`.

#![cfg(feature = "testing")]

mod stream_support;

use std::sync::Arc;

use actix_web::{App, test};
use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;
use rustak_api::identity::Direction;
use rustak_cot::codec::{EncodedEvent, Mode};
use rustak_cot::detail::flow_tags;
use rustak_cot::{CotTime, Event};
use rustak_server::config::TEST_SERVER_NAME;
use rustak_server::cot_store::{CotRecord, history::HistoryWriter, latest};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use serde_json::Value;

use stream_support::{EXPECT, Harness, settle};

/// A full member of a channel.
const BOTH: Direction = Direction::Both;

/// The channel the stored-row tests file everything under.
const CHANNEL: &str = "blue";

/// The document production actually emitted, for the reader's own self-test.
const AS_IT_WENT_OUT: &str = concat!(
    r#"<event version="2.0" uid="UID-A" type="a-f-G-U-C"><point lat="51.5" lon="-0.12"/>"#,
    r#"<detail><_flow-tags_ TAK-Server-SierraSoftworks TAK="2026-09-19T00:00:00.000Z"/>"#,
    r#"</detail></event>"#,
);

/// What a parser that owes us nothing makes of a document.
///
/// [`None`] when it reads cleanly; otherwise the first thing it refused, which
/// is what somebody debugging a failure needs and an `assert!` would not give
/// them. Three things are checked, and the production document fails the
/// second:
///
/// 1. The reader itself, with end-name checking on, reaches the end.
/// 2. Every attribute parses — quick-xml's checked iterator refuses
///    `name value="x"` exactly as sax refuses "Attribute without value".
/// 3. Every element and attribute name is an XML 1.0 `Name`. A reader will
///    happily hand back a name beginning with a digit; a writer may not emit
///    one, and the next parser along may refuse it.
fn strict_refusal(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;

    loop {
        let tag = match reader.read_event() {
            Ok(XmlEvent::Eof) => return None,
            Ok(XmlEvent::Start(tag) | XmlEvent::Empty(tag)) => tag,
            Ok(_) => continue,
            Err(err) => return Some(format!("the reader refused the document: {err}")),
        };

        let name = tag.name().into_inner().to_string();

        if !rustak_cot::xml::is_name(&name) {
            return Some(format!("<{name}> is not an XML name"));
        }

        for attribute in tag.attributes() {
            let attribute = match attribute {
                Ok(attribute) => attribute,
                Err(err) => return Some(format!("<{name}> has an unreadable attribute: {err}")),
            };

            let key = attribute.key.into_inner().to_string();

            if !rustak_cot::xml::is_name(&key) {
                return Some(format!(
                    "<{name}> carries '{key}', which is not an XML name"
                ));
            }
        }
    }
}

// `actix_web::test` is in scope as both a module and an attribute macro, so a
// bare `#[test]` here would resolve to actix's and ask for an `async fn`.
#[actix_web::test]
async fn the_reader_these_tests_trust_refuses_what_production_sent() {
    // If this passes, every assertion below is worthless — so it is asserted
    // rather than assumed, with the exact document the outage put on the wire.
    let refusal = strict_refusal(AS_IT_WENT_OUT).expect("the production document must be refused");

    assert!(
        refusal.contains("_flow-tags_"),
        "the refusal should name the element that carried the bad attribute: {refusal}",
    );

    // And a name that reads cleanly but may not be written is caught too.
    assert!(strict_refusal(r#"<detail><2nd a="b"/></detail>"#).is_some());
    assert!(strict_refusal(r#"<detail><second a="b"/></detail>"#).is_none());
}

#[tokio::test]
async fn a_protobuf_peer_and_an_xml_peer_both_read_a_relay_from_a_hostilely_named_server() {
    let harness = Harness::start().await;

    assert_eq!(
        harness.context.config().server.name,
        TEST_SERVER_NAME,
        "this whole file is about relaying from an installation called that",
    );

    let alice = harness
        .enroll("alice", "UID-ALICE", &[(CHANNEL, BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[(CHANNEL, BOTH)]).await;
    let carol = harness
        .enroll("carol", "UID-CAROL", &[(CHANNEL, BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut charlie = harness
        .eud_with(&carol, "CHARLIE", |config| config.with_negotiation(false))
        .await;
    harness.await_connected(3).await;

    settle(&mut bravo).await;
    settle(&mut charlie).await;
    assert_eq!(bravo.stream().mode(), Mode::Proto, "BRAVO negotiated");
    assert_eq!(charlie.stream().mode(), Mode::Xml, "CHARLIE never did");

    alpha.send_sa(51.5, -0.12).await.unwrap();

    let over_proto = bravo
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("the protobuf peer is served");
    let over_xml = charlie
        .expect(|event| event.uid == "UID-ALICE", EXPECT)
        .await
        .expect("the XML peer is served");

    // Arriving at all is most of the assertion for the XML peer: the frame it
    // read came off the socket as bytes and through `rustak_cot::xml::parse`,
    // which has refused names outside the `Name` production since 9c6f6f4. A
    // relay stamped `TAK-Server-Rustak Test & Co. (näme)` would never have
    // become an `Event` here.
    for (peer, seen) in [("BRAVO", &over_proto), ("CHARLIE", &over_xml)] {
        assert!(
            flow_tags::has_flow_tag(&seen.detail, TEST_SERVER_NAME),
            "{peer} should have been handed this server's flow tag",
        );

        let stamped = flow_tags::flow_tag_name(TEST_SERVER_NAME);

        assert!(
            rustak_cot::xml::is_name(&stamped),
            "the attribute name derived from the display name is not an XML name: {stamped}",
        );
        assert!(
            !stamped.contains(' ') && !stamped.contains('&') && !stamped.contains('('),
            "the derivation left a character an XML name may not carry: {stamped}",
        );

        if let Some(refusal) =
            strict_refusal(&String::from_utf8_lossy(&rustak_cot::xml::write(seen)))
        {
            panic!("what {peer} received does not survive a strict reader: {refusal}");
        }
    }

    harness.stop().await;
}

/// Stores one row exactly as the relay path leaves it: flow tag and all.
///
/// Both halves of the store, because the two documents under test read
/// different ones: `/Marti/api/cot/xml/{uid}/all` replays the append-only
/// history segments (which hold the **protobuf** encoding, so the document is
/// built by decoding it back), and a mission's `/cot` reads the latest row's
/// XML. A regression that only reached one of them would otherwise pass here.
async fn store_relayed(server: &TestServer, uid: &str, bitpos: u32) {
    let mut event = Event::builder("a-f-G-U-C", uid)
        .how("m-g")
        .point(51.5, -0.12)
        .time(CotTime::now())
        .build();

    // The same call `stream::Router` makes on the way out, with the same id.
    flow_tags::add_flow_tag(
        &mut event.detail,
        &server.context.config().server.name,
        CotTime::now(),
    );

    let mut groups = GroupSet::new();
    groups.set(bitpos, Direction::In);

    let principal = Principal::new(
        UserId::from(1),
        Username::parse("sender").unwrap(),
        PrincipalKind::Person,
        AuthMethod::SetupToken,
    )
    .with_groups(Arc::new(groups));

    let record = CotRecord {
        user_id: None,
        ..CotRecord::new(Arc::new(EncodedEvent::new(event)), &principal, None)
    };

    let mut history = HistoryWriter::new(
        server.context.db().clone(),
        server.context.config().streams_dir(),
    );

    history
        .append(&record.uid, record.received_at, record.proto())
        .await
        .expect("append the relayed message to the history");
    history.flush().await.expect("the index catches up");

    latest::upsert_batch(server.context.db(), vec![record])
        .await
        .expect("store a relayed message");
}

/// An account that receives on [`CHANNEL`], the channel's bit, and the token.
async fn reader(server: &TestServer) -> (u32, String) {
    let name = GroupName::parse(CHANNEL).unwrap();
    let db = server.context.db();
    let group = db
        .groups()
        .create(rustak_server::db::repos::NewGroup::manual(name))
        .await
        .unwrap();

    let (user, session) = server.signed_in("ada", false).await;

    db.members()
        .grant(
            user.id,
            group.id,
            Direction::Both,
            rustak_api::MembershipSource::Manual,
        )
        .await
        .unwrap();

    (group.bitpos, session.token)
}

#[actix_web::test]
async fn the_uid_history_document_is_one_a_strict_reader_takes_whole() {
    let server = TestServer::start().await;
    let (bitpos, token) = reader(&server).await;

    store_relayed(&server, "UID-RELAYED", bitpos).await;

    let app = test::init_service(App::new().configure(server.app())).await;
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/cot/xml/UID-RELAYED/all?secago=3600")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert!(body.contains("<events"), "{body}");
    assert!(
        body.contains("_flow-tags_"),
        "the row's tag is served: {body}"
    );
    assert!(
        body.contains(&flow_tags::flow_tag_name(TEST_SERVER_NAME)),
        "and it is the derived name, not the display name: {body}",
    );

    if let Some(refusal) = strict_refusal(&body) {
        panic!("the uid history is not a document CloudTAK could read: {refusal}\n{body}");
    }
}

#[actix_web::test]
async fn a_missions_cot_document_is_one_a_strict_reader_takes_whole() {
    // The document the outage answered `500` for: a mission holding one item
    // is enough, because it is parsed as a whole and one bad row takes the
    // rest of it down.
    let server = TestServer::start().await;
    let (bitpos, token) = reader(&server).await;

    let app = test::init_service(App::new().configure(server.app())).await;
    let created = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Alpha%20Team?creatorUid=ANDROID-owner")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;

    assert_eq!(created.status().as_u16(), 201);

    let body: Value = test::read_body_json(created).await;
    let guid = body["data"][0]["guid"].as_str().unwrap().to_string();
    let mission_id = server
        .context
        .db()
        .missions()
        .by_guid(guid.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .id;

    store_relayed(&server, "UID-FILED", bitpos).await;

    server
        .context
        .db()
        .mission_contents()
        .upsert_uid(rustak_server::db::repos::MissionUidRow::new(
            mission_id,
            "UID-FILED".to_string(),
            chrono::Utc::now(),
        ))
        .await
        .unwrap();

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/api/missions/guid/{guid}/cot"))
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "a mission holding one relayed item still answers its document",
    );

    let document = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert!(document.contains("uid=\"UID-FILED\""), "{document}");
    assert!(document.contains("_flow-tags_"), "{document}");

    if let Some(refusal) = strict_refusal(&document) {
        panic!("the mission document is one CloudTAK would answer 500 for: {refusal}\n{document}");
    }
}
