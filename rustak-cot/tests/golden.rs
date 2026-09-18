//! Golden tests: every hand-written fixture in `fixtures/` must survive a
//! parse/write cycle, and the bytes it produces are pinned in `tests/golden/`.
//!
//! Set `RUSTAK_UPDATE_GOLDEN=1` to rewrite the pinned outputs after a
//! deliberate change to the writer.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rstest::rstest;
use rustak_cot::detail::{
    AckRequest, Chat, Contact, Dest, DestKind, FileShare, Group, Link, MissionDetail,
    MissionNotice, PrecisionLocation, Status, StrictDetail, TakControl, Takv, Track, flow_tags,
    marti,
};
use rustak_cot::{CotTime, Detail, Event, Node, xml};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn load(name: &str) -> Event {
    let bytes = std::fs::read(fixtures_dir().join(format!("{name}.xml")))
        .unwrap_or_else(|err| panic!("read fixture {name}: {err}"));
    xml::parse(&bytes).unwrap_or_else(|err| panic!("parse fixture {name}: {err}"))
}

/// The characters CloudTAK strips before parsing, which we must never emit.
fn has_forbidden_controls(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
}

const FIXTURES: &[&str] = &[
    "cdata-remarks",
    "chat-direct",
    "chat-room",
    "detail-self-closing",
    "disconnect",
    "fileshare",
    "mission-change",
    "no-detail",
    "ping",
    "pong",
    "sa-full",
    "sa-lenient",
    "single-quotes",
    "takp-q",
    "takp-r",
    "takp-v",
    "unicode-entities",
];

#[test]
fn every_fixture_writes_the_pinned_bytes() {
    for name in FIXTURES {
        let event = load(name);
        let written = xml::write(&event);
        let text = std::str::from_utf8(&written).expect("the writer emits UTF-8");
        let path = golden_dir().join(format!("{name}.xml"));

        if std::env::var_os("RUSTAK_UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &written).expect("write golden");
            continue;
        }

        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read golden {name}: {err} (RUSTAK_UPDATE_GOLDEN=1)"));
        assert_eq!(text, expected, "golden mismatch for {name}");
    }
}

#[rstest]
fn every_fixture_round_trips_without_losing_meaning(
    #[values(
        "cdata-remarks",
        "chat-direct",
        "chat-room",
        "detail-self-closing",
        "disconnect",
        "fileshare",
        "mission-change",
        "no-detail",
        "ping",
        "pong",
        "sa-full",
        "sa-lenient",
        "single-quotes",
        "takp-q",
        "takp-r",
        "takp-v",
        "unicode-entities"
    )]
    name: &str,
) {
    let event = load(name);
    let written = xml::write(&event);
    let reparsed = xml::parse(&written).expect("our own output parses");
    assert_eq!(reparsed, event, "{name} lost meaning on the round trip");

    // Writing twice is idempotent, so a relayed message is byte-stable.
    assert_eq!(xml::write(&reparsed), written, "{name} is not idempotent");
}

#[rstest]
fn every_fixture_obeys_the_outbound_wire_contract(
    #[values(
        "cdata-remarks",
        "chat-direct",
        "chat-room",
        "detail-self-closing",
        "disconnect",
        "fileshare",
        "mission-change",
        "no-detail",
        "ping",
        "pong",
        "sa-full",
        "sa-lenient",
        "single-quotes",
        "takp-q",
        "takp-r",
        "takp-v",
        "unicode-entities"
    )]
    name: &str,
) {
    let written = xml::write(&load(name));
    let text = std::str::from_utf8(&written).unwrap();

    assert!(
        text.starts_with(&format!("{}\n<event ", xml::DECLARATION)),
        "{name}: declaration must be followed by exactly one newline"
    );
    assert!(text.ends_with("</event>"), "{name}: no trailing newline");
    assert!(!text.contains("<event/>"), "{name}: never self-closing");
    assert!(
        !has_forbidden_controls(&text[xml::DECLARATION.len() + 1..]),
        "{name}: control characters would be stripped by CloudTAK"
    );
    assert_eq!(text.matches("<event ").count(), 1, "{name}: one event only");
    assert_eq!(
        text.matches("</event>").count(),
        1,
        "{name}: one event only"
    );
}

#[test]
fn the_situational_awareness_fixture_exposes_every_typed_detail() {
    let event = load("sa-full");
    assert!(event.is_sa());
    assert_eq!(event.callsign(), Some("ALPHA"));
    assert_eq!(event.endpoint(), Some("*:-1:stcp"));

    let detail = &event.detail;
    assert!(Contact::strict(detail.find("contact").unwrap()).is_some());
    assert!(Group::strict(detail.find("__group").unwrap()).is_some());
    assert!(PrecisionLocation::strict(detail.find("precisionlocation").unwrap()).is_some());
    assert!(Status::strict(detail.find("status").unwrap()).is_some());
    assert!(Takv::strict(detail.find("takv").unwrap()).is_some());
    assert!(Track::strict(detail.find("track").unwrap()).is_some());

    assert_eq!(detail.get::<Status>().unwrap().battery, 87);
    assert_eq!(detail.get::<Track>().unwrap().course, 270.0);
    assert_eq!(detail.get::<Takv>().unwrap().summary(), "rustak-test:0.1.0");
    // Unmodelled children survive untouched.
    assert_eq!(detail.find("uid").unwrap().get("Droid"), Some("ALPHA"));
    assert!(flow_tags::has_flow_tag(detail, "rustak-test"));
    assert!(!flow_tags::has_flow_tag(detail, "someone-else"));
}

#[test]
fn the_lenient_fixture_reads_but_never_converts_strictly() {
    let event = load("sa-lenient");
    let detail = &event.detail;

    // Every one of these would be rewritten by a lax converter; the strict
    // rules leave them in xmlDetail instead.
    assert!(Contact::strict(detail.find("contact").unwrap()).is_none());
    assert!(Group::strict(detail.find("__group").unwrap()).is_none());
    assert!(Status::strict(detail.find("status").unwrap()).is_none());
    assert!(Track::strict(detail.find("track").unwrap()).is_none());
    assert!(PrecisionLocation::strict(detail.find("precisionlocation").unwrap()).is_none());

    // The lenient views still answer.
    assert_eq!(event.callsign(), Some("BRAVO"));
    assert_eq!(detail.get::<Status>().unwrap().battery, 0);
    assert_eq!(detail.get::<Track>().unwrap().speed, 0.0);
    assert_eq!(
        event.extra_attrs,
        vec![("vendor".to_owned(), "rustak".to_owned())]
    );
    // hae/ce/le absent from <point> fall back to the unknown sentinel.
    assert_eq!(event.point.hae, 9_999_999.0);
}

#[test]
fn chat_fixtures_carry_the_attribute_sets_atak_builds() {
    let room = load("chat-room");
    let chat = room.detail.get::<Chat>().unwrap();
    assert_eq!(chat.room(), "All Chat Rooms");
    assert_eq!(chat.message_id.as_deref(), Some("MSG-0001"));
    assert_eq!(chat.sender_callsign.as_deref(), Some("ALPHA"));
    assert_eq!(
        chat.participants(),
        vec!["ANDROID-rustak-alpha", "All Chat Rooms"]
    );
    let remarks = rustak_cot::detail::chat::remarks(&room.detail).unwrap();
    assert_eq!(remarks.text, "radio check, all stations");
    assert_eq!(remarks.to, None, "room chat never carries a `to`");
    assert_eq!(
        room.detail.get::<Link>().unwrap().relation.as_deref(),
        Some("p-p")
    );

    let direct = load("chat-direct");
    let remarks = rustak_cot::detail::chat::remarks(&direct.detail).unwrap();
    assert_eq!(remarks.to.as_deref(), Some("ANDROID-rustak-bravo"));
}

#[test]
fn the_direct_chat_fixture_addresses_a_callsign_and_marti_is_strippable() {
    let mut event = load("chat-direct");
    assert_eq!(marti::read_marti(&event.detail).len(), 1);

    let dests = marti::take_marti(&mut event.detail);
    assert_eq!(dests.len(), 1);
    assert_eq!(dests[0].kind(), Some(DestKind::Callsign("BRAVO")));
    assert!(!dests[0].is_all_streaming());
    assert_eq!(event.detail.count("marti"), 0);

    // A relayed copy no longer names anyone.
    let relayed = String::from_utf8(xml::write(&event).to_vec()).unwrap();
    assert!(!relayed.contains("<marti"));
    assert!(!relayed.contains("<dest"));
}

#[test]
fn the_fileshare_fixture_matches_the_verified_attribute_set() {
    let event = load("fileshare");
    assert_eq!(event.r#type, "b-f-t-r");
    assert_eq!(event.how.as_deref(), Some("h-e"));
    assert_eq!(event.stale - event.start, 10_000);

    let share = event.detail.get::<FileShare>().unwrap();
    assert_eq!(share.size_in_bytes, 4_096);
    assert_eq!(share.sender_callsign, "ALPHA");
    assert!(share.sender_url.contains("/Marti/sync/content"));

    let ack = event.detail.get::<AckRequest>().unwrap();
    assert!(ack.ack_requested);
    assert_eq!(ack.tag, "Route Package");
}

#[test]
fn the_negotiation_fixtures_read_as_a_complete_handshake() {
    let announce = load("takp-v").detail.get::<TakControl>().unwrap();
    assert!(announce.supports(1));
    assert_eq!(announce.api_version, Some(3));
    assert_eq!(
        announce.server_version.as_deref(),
        Some("TAK Server rustak-0.1.0")
    );

    assert_eq!(
        load("takp-q").detail.get::<TakControl>().unwrap().request,
        Some(1)
    );
    assert_eq!(
        load("takp-r").detail.get::<TakControl>().unwrap().response,
        Some(true)
    );
}

#[test]
fn the_keepalive_fixtures_carry_a_point_and_the_pong_has_no_detail() {
    let ping = load("ping");
    assert_eq!(ping.r#type, "t-x-c-t");
    assert!(ping.uid.ends_with("-ping"));
    assert!(ping.is_control());

    let pong = load("pong");
    assert_eq!(pong.uid, "takPong");
    assert_eq!(pong.how.as_deref(), Some("h-g-i-g-o"));
    assert!(pong.detail.is_empty(), "the pong carries no detail at all");
    assert_eq!(pong.point.lat, 0.0);
    assert_eq!(pong.point.ce, 9_999_999.0);
    // Single quotes and the stray space before `/>` normalise away.
    let text = String::from_utf8(xml::write(&pong).to_vec()).unwrap();
    assert!(text.contains(r#"uid="takPong""#));
    assert!(!text.contains("<detail"));
}

#[test]
fn the_disconnect_fixture_names_the_departing_peer() {
    let event = load("disconnect");
    assert_eq!(event.r#type, "t-x-d-d");
    assert!(!event.is_control(), "t-x-d-d is brokered, not consumed");
    let link = rustak_cot::detail::link::links(&event.detail);
    assert_eq!(link.len(), 1);
    assert_eq!(link[0].uid.as_deref(), Some("ANDROID-rustak-bravo"));
    assert_eq!(link[0].r#type.as_deref(), Some("a-f-G-U-C"));
    assert_eq!(event.stale - event.start, 20_000);
}

#[test]
fn the_mission_fixture_reads_its_child_element_payload() {
    let event = load("mission-change");
    let mission = event.detail.get::<MissionDetail>().unwrap();
    assert_eq!(mission.r#type, MissionNotice::Change);
    assert_eq!(mission.name.as_deref(), Some("Recon"));
    assert_eq!(mission.changes.len(), 1);

    let change = &mission.changes[0];
    assert_eq!(change.r#type.as_deref(), Some("ADD_CONTENT"));
    assert_eq!(change.is_federated_change, Some(false));
    assert_eq!(
        change.content_uid.as_deref(),
        Some("ANDROID-rustak-alpha-marker-1")
    );
}

#[test]
fn unicode_entities_and_cdata_survive_verbatim() {
    let event = load("unicode-entities");
    assert_eq!(event.callsign(), Some("CHARLIE & CÅT 🛰️"));
    let remarks = rustak_cot::detail::chat::remarks(&event.detail).unwrap();
    assert_eq!(
        remarks.text,
        r#"say "again" <over> & out — ☕ &unknownentity;"#
    );

    let cdata = load("cdata-remarks");
    assert_eq!(
        cdata.detail.find("remarks").unwrap().children,
        vec![Node::CData("grid <51.5074, -0.1278> & holding".to_owned())]
    );
    assert!(
        cdata
            .detail
            .nodes
            .iter()
            .any(|node| matches!(node, Node::Comment(_)))
    );
}

#[test]
fn an_absent_and_a_self_closing_detail_are_indistinguishable() {
    let absent = load("no-detail");
    let self_closing = load("detail-self-closing");
    assert!(absent.detail.is_empty());
    assert!(self_closing.detail.is_empty());
    for event in [&absent, &self_closing] {
        let text = String::from_utf8(xml::write(event).to_vec()).unwrap();
        assert!(!text.contains("<detail"));
    }
}

#[test]
fn a_seventy_kibibyte_remarks_message_round_trips() {
    let body = "the quick brown fox jumps over the lazy dog. ".repeat(1_600);
    assert!(body.len() > 70 * 1024);

    let mut detail = Detail::new();
    detail.push(
        rustak_cot::Element::new("remarks")
            .attr("source", "BAO.F.rustak.ANDROID-rustak-alpha")
            .with(Node::Text(body.clone())),
    );
    let event = Event::builder("b-t-f", "GeoChat.ANDROID-rustak-alpha.room.big")
        .how("h-g-i-g-o")
        .point(51.5074, -0.1278)
        .time(CotTime::from_millis(1_789_646_400_000))
        .stale_after(std::time::Duration::from_secs(86_400))
        .detail(detail)
        .build();

    let written = xml::write(&event);
    assert!(written.len() > 70 * 1024);
    assert_eq!(xml::parse(&written).unwrap(), event);
    assert_eq!(
        xml::parse(&written)
            .unwrap()
            .detail
            .find("remarks")
            .unwrap()
            .text(),
        body
    );
}

#[test]
fn a_dest_addressed_at_all_streaming_degrades_to_a_broadcast() {
    let mut event = load("chat-room");
    event.detail.push(marti::marti_element(&[Dest::callsign(
        marti::ALL_STREAMING,
    )]));
    let dests = marti::take_marti(&mut event.detail);
    assert!(dests[0].is_all_streaming());
    assert_eq!(dests[0].kind(), Some(DestKind::Callsign("All Streaming")));
}

#[test]
fn a_relay_adds_its_flow_tag_without_disturbing_anything_else() {
    let mut event = load("chat-room");
    let before = event.detail.nodes.len();
    flow_tags::add_flow_tag(
        &mut event.detail,
        "rustak-test",
        CotTime::from_millis(1_789_646_400_000),
    );
    assert_eq!(event.detail.nodes.len(), before + 1);
    assert!(flow_tags::has_flow_tag(&event.detail, "rustak-test"));

    let text = String::from_utf8(xml::write(&event).to_vec()).unwrap();
    assert!(text.contains(r#"<_flow-tags_ TAK-Server-rustak-test="2026-09-17T12:00:00.000Z"/>"#));
    assert_eq!(xml::parse(text.as_bytes()).unwrap(), event);
}
