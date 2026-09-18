//! `<dest mission=…>`: the write, the relay, and the notice that follows it.
//!
//! Two halves, because the feature has two halves. The **templates** are pinned
//! byte for byte against `tests/golden/missions/`: a `t-x-m-*` notice is parsed
//! by CloudTAK's own wire types and by ATAK's Data Sync plugin, and a drifted
//! attribute is a feature that silently stops updating rather than an error
//! anybody sees. The **routing** is driven through a real listener with real
//! enrolled clients, because "the subscriber got it and the sender did not" is
//! not a property a unit test of the publisher can observe.
//!
//! Set `RUSTAK_UPDATE_GOLDEN=1` to rewrite the pinned templates after a
//! deliberate change.

mod stream_support;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rustak_api::MissionRoleKind;
use rustak_api::identity::GroupName;
use rustak_cot::{CotTime, Event, xml};
use rustak_server::prelude::Services as _;
use rustak_server::stream::mission_notify::{
    ChangeKind, MissionNotice, NoticeMission, Recipients, events_with,
};
use rustak_server::stream::mission_payload::{
    MissionChangeXml, MissionLayerXml, MissionRoleXml, ResourceXml, UidDetailsXml,
};

/// `2026-09-17T12:00:40.000Z`, the instant every pinned template is rendered at.
const NOW: CotTime = CotTime::from_millis(1_789_646_440_000);

/// The same instant as a wall clock, for the payload timestamps.
fn at() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_789_646_440_000).expect("a valid instant")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/missions")
}

fn mission() -> NoticeMission {
    NoticeMission {
        name: "Operation Kettle".into(),
        guid: "0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f".into(),
        tool: "public".into(),
        groups: vec![GroupName::parse("blue").expect("a usable channel name")],
    }
}

/// Renders a notice with pinned event uids and compares it to its golden file.
fn assert_golden(name: &str, notice: &MissionNotice) {
    let mut next = 0;
    let events = events_with(notice, NOW, || {
        next += 1;
        format!("11111111-2222-3333-4444-00000000000{next}")
    });

    let rendered = events
        .iter()
        .map(|event: &Event| {
            String::from_utf8(xml::write(event).to_vec()).expect("the writer emits UTF-8")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let path = golden_dir().join(format!("{name}.xml"));

    if std::env::var("RUSTAK_UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(golden_dir()).expect("a directory for the pinned templates");
        std::fs::write(&path, format!("{rendered}\n")).expect("rewrite the pinned template");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "read {}: {err} (RUSTAK_UPDATE_GOLDEN=1 writes it)",
            path.display()
        )
    });

    assert_eq!(
        rendered,
        expected.trim_end_matches('\n'),
        "{name} drifted from its pinned template",
    );
}

fn change(kind: ChangeKind, changes: Vec<MissionChangeXml>) -> MissionNotice {
    MissionNotice::Change {
        kind,
        mission: mission(),
        author_uid: Some("ANDROID-AUTHOR".into()),
        changes,
        layer: None,
        recipients: Recipients::Subscribers(vec!["ANDROID-READER".into()]),
    }
}

fn resource() -> ResourceXml {
    ResourceXml {
        creator_uid: "ANDROID-AUTHOR".into(),
        expiration: -1,
        filename: "plan.pdf".into(),
        hash: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
        keywords: vec!["missionpackage".into()],
        mime_type: "application/pdf".into(),
        name: "Plan".into(),
        size: 4096,
        submission_time: at(),
        submitter: "alice".into(),
        tool: "public".into(),
        uid: "RESOURCE-1".into(),
    }
}

fn uid_change(kind: &str) -> MissionChangeXml {
    let mut change = MissionChangeXml::new(kind, at());
    change.content_uid = Some("UID-MARKER".into());
    change.creator_uid = Some("ANDROID-AUTHOR".into());
    change.details = Some(UidDetailsXml {
        kind: Some("a-f-G-U-C".into()),
        callsign: Some("ALPHA".into()),
        title: Some("Rally point".into()),
        iconset_path: Some("COT_MAPPING_2525B/a-f/a-f-G".into()),
        color: Some("-1".into()),
        location: Some((51.5, -0.12)),
    });

    change
}

#[test]
fn a_content_add_matches_its_pinned_template() {
    let mut added = MissionChangeXml::new("ADD_CONTENT", at());
    added.creator_uid = Some("ANDROID-AUTHOR".into());
    added.resource = Some(resource());

    assert_golden(
        "t-x-m-c-add-content",
        &change(ChangeKind::Content, vec![added]),
    );
}

#[test]
fn a_uid_add_matches_its_pinned_template() {
    assert_golden(
        "t-x-m-c-add-uid",
        &change(ChangeKind::Content, vec![uid_change("ADD_CONTENT")]),
    );
}

#[test]
fn a_content_removal_matches_its_pinned_template() {
    assert_golden(
        "t-x-m-c-remove-content",
        &change(ChangeKind::Content, vec![uid_change("REMOVE_CONTENT")]),
    );
}

#[test]
fn every_keyword_and_metadata_notice_matches_its_pinned_template() {
    for (kind, name) in [
        (ChangeKind::Log, "t-x-m-c-l-log"),
        (ChangeKind::Keyword, "t-x-m-c-k-keywords"),
        (ChangeKind::UidKeyword, "t-x-m-c-k-u-uid-keywords"),
        (ChangeKind::ResourceKeyword, "t-x-m-c-k-c-resource-keywords"),
        (ChangeKind::Metadata, "t-x-m-c-m-metadata"),
        (ChangeKind::ExternalData, "t-x-m-c-e-external-data"),
    ] {
        assert_golden(name, &change(kind, Vec::new()));
    }
}

#[test]
fn a_layer_change_matches_its_pinned_template() {
    assert_golden(
        "t-x-m-c-h-layer",
        &MissionNotice::Change {
            kind: ChangeKind::Layer,
            mission: mission(),
            author_uid: Some("ANDROID-AUTHOR".into()),
            changes: Vec::new(),
            layer: Some(MissionLayerXml {
                uid: "layer-1".into(),
                name: Some("Markers".into()),
                kind: "UID".into(),
                parent_uid: Some("layer-root".into()),
            }),
            recipients: Recipients::Subscribers(vec!["ANDROID-READER".into()]),
        },
    );
}

#[test]
fn the_create_and_delete_announcements_match_their_pinned_templates() {
    assert_golden(
        "t-x-m-n-created",
        &MissionNotice::Created {
            mission: mission(),
            author_uid: Some("ANDROID-AUTHOR".into()),
        },
    );
    assert_golden(
        "t-x-m-d-deleted",
        &MissionNotice::Deleted {
            mission: mission(),
            author_uid: Some("ANDROID-AUTHOR".into()),
        },
    );
}

#[test]
fn the_invite_and_role_change_match_their_pinned_templates() {
    assert_golden(
        "t-x-m-i-invite",
        &MissionNotice::Invite {
            mission: mission(),
            author_uid: Some("ANDROID-AUTHOR".into()),
            token: "eyJhbGciOiJIUzI1NiJ9.invitation.signature".into(),
            role: MissionRoleXml(MissionRoleKind::Subscriber),
            uids: vec!["ANDROID-READER".into()],
        },
    );
    assert_golden(
        "t-x-m-r-role-change",
        &MissionNotice::RoleChange {
            mission: mission(),
            author_uid: Some("ANDROID-AUTHOR".into()),
            role: MissionRoleXml(MissionRoleKind::ReadonlySubscriber),
            uid: "ANDROID-READER".into(),
        },
    );
}

/// The device that owns the mission and writes into it.
const WRITER_UID: &str = "UID-WRITER";

/// The device subscribed to it that should hear everything.
const READER_UID: &str = "UID-READER";

/// The device that is not subscribed at all.
const STRANGER_UID: &str = "UID-STRANGER";

/// Creates a mission and subscribes the given devices with the given roles.
async fn data_sync(harness: &stream_support::Harness, subscribers: &[(&str, &str)]) -> i64 {
    let db = harness.context.db();
    let mission = db
        .missions()
        .create(rustak_server::db::repos::NewMission::new(
            "Kettle",
            "MISSION_SUBSCRIBER",
        ))
        .await
        .expect("the mission under test");

    for (uid, role) in subscribers {
        db.mission_subscriptions()
            .upsert(rustak_server::db::repos::NewSubscription {
                mission_id: mission.id,
                subscription_uid: uuid::Uuid::new_v4().to_string(),
                client_uid: (*uid).to_string(),
                username: None,
                role: (*role).to_string(),
                token_jti: None,
            })
            .await
            .expect("a subscription");
    }

    mission.id
}

/// A client of the harness, shortened for the helpers below.
type Eud = rustak_client::stream::testing::Eud;

/// Announces every client, then drains what that announcement caused.
///
/// A connection is only in the hub's uid index once it has said what it calls
/// itself, and a mission notice is addressed by uid — so a subscriber that has
/// never sent an SA message cannot be reached by one. The drain is a
/// [`barrier`] per sender rather than a sleep: messages reach one connection in
/// the order the server wrote them, so seeing a sender's barrier means having
/// already seen everything that sender caused before it.
async fn announce(euds: &mut [&mut Eud]) {
    for eud in euds.iter_mut() {
        eud.send_sa(51.5, -0.12).await.expect("an SA message");
    }

    for sender in 0..euds.len() {
        let uid = format!("UID-BARRIER-{sender}");

        euds[sender].send(plain(&uid)).await.expect("a barrier");

        for (index, eud) in euds.iter_mut().enumerate() {
            if index != sender {
                eud.expect_uid(&uid, stream_support::EXPECT)
                    .await
                    .expect("every other client sees the barrier");
            }
        }
    }
}

/// Sends a barrier and answers what the client saw first.
///
/// The assertion every negative case here makes: a client that sees the
/// barrier before it sees the mission traffic is a client the mission traffic
/// never reached, and it says so without waiting on a clock.
async fn first_after(from: &mut Eud, uid: &str, watcher: &mut Eud) -> rustak_cot::Event {
    from.send(plain(uid)).await.expect("a barrier");

    let wanted = uid.to_string();

    watcher
        .expect(
            move |event| {
                event.uid == wanted
                    || event.uid == "UID-MARKER"
                    || event.r#type.starts_with("t-x-m-c")
            },
            stream_support::EXPECT,
        )
        .await
        .expect("the barrier, or whatever arrived before it")
}

/// An ordinary broadcast with no `<marti>` at all.
fn plain(uid: &str) -> rustak_cot::Event {
    rustak_cot::Event::builder("a-f-G-U-C", uid)
        .point(51.5, -0.12)
        .stale_after(std::time::Duration::from_secs(60))
        .build()
}

/// An event addressed at a mission by name.
fn addressed(uid: &str) -> rustak_cot::Event {
    let mut event = rustak_cot::Event::builder("a-f-G-U-C", uid)
        .point(51.5, -0.12)
        .stale_after(std::time::Duration::from_secs(60))
        .build();

    event
        .detail
        .push(rustak_cot::detail::marti::marti_element(&[
            rustak_cot::detail::Dest::mission("Kettle"),
        ]));

    event
}

#[tokio::test]
async fn a_subscriber_gets_the_message_and_the_notice_and_the_sender_gets_neither() {
    // Two deliveries, by two routes: the raw CoT is relayed to the mission's
    // subscribers as an explicit-uid hit, and a `t-x-m-c` describing the change
    // is pushed at the same people. Both happen; neither replaces the other.
    let harness = stream_support::Harness::start_with_missions().await;
    let writer = harness
        .enroll(
            "writer",
            WRITER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;
    let reader = harness
        .enroll(
            "reader",
            READER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;
    let stranger = harness
        .enroll(
            "stranger",
            STRANGER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;

    data_sync(
        &harness,
        &[
            (WRITER_UID, "MISSION_SUBSCRIBER"),
            (READER_UID, "MISSION_SUBSCRIBER"),
        ],
    )
    .await;

    let mut writing = harness.eud(&writer, "WRITER").await;
    let mut reading = harness.eud(&reader, "READER").await;
    let mut watching = harness.eud(&stranger, "STRANGER").await;
    harness.await_connected(3).await;
    announce(&mut [&mut writing, &mut reading, &mut watching]).await;

    writing.send(addressed("UID-MARKER")).await.unwrap();

    // The notice first: it is sent while the message is being filed, which
    // happens before the router fans the message itself out.
    let notice = reading
        .expect(
            |event| event.r#type.starts_with("t-x-m-c"),
            stream_support::EXPECT,
        )
        .await
        .expect("the subscriber is told about the change");

    let relayed = reading
        .expect(|event| event.uid == "UID-MARKER", stream_support::EXPECT)
        .await
        .expect("and is sent the message itself");
    assert_eq!(relayed.r#type, "a-f-G-U-C");
    let mission = notice.detail.find("mission").expect("a <mission> child");
    assert_eq!(mission.get("name"), Some("Kettle"));
    assert_eq!(mission.get("type"), Some("CHANGE"));
    assert_eq!(
        mission
            .child("MissionChanges")
            .and_then(|changes| changes.child("MissionChange"))
            .and_then(|change| change.child("contentUid"))
            .map(rustak_cot::detail::Element::text)
            .as_deref(),
        Some("UID-MARKER"),
    );

    assert!(
        watching.expect_none(stream_support::SETTLE).await.is_ok(),
        "a device that is not subscribed hears nothing",
    );
    assert!(
        writing.expect_none(stream_support::SETTLE).await.is_ok(),
        "the sender already knows what it sent",
    );
}

#[tokio::test]
async fn a_sender_with_no_subscription_files_nothing_and_reaches_nobody() {
    let harness = stream_support::Harness::start_with_missions().await;
    let writer = harness
        .enroll(
            "writer",
            WRITER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;
    let reader = harness
        .enroll(
            "reader",
            READER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;

    let mission = data_sync(&harness, &[(READER_UID, "MISSION_SUBSCRIBER")]).await;

    let mut writing = harness.eud(&writer, "WRITER").await;
    let mut reading = harness.eud(&reader, "READER").await;
    harness.await_connected(2).await;
    announce(&mut [&mut writing, &mut reading]).await;

    writing.send(addressed("UID-MARKER")).await.unwrap();

    assert_eq!(
        first_after(&mut writing, "UID-BARRIER-DROPPED", &mut reading)
            .await
            .uid,
        "UID-BARRIER-DROPPED",
        "the message was dropped for that mission",
    );
    assert!(
        harness
            .context
            .db()
            .mission_contents()
            .uids(mission)
            .await
            .unwrap()
            .is_empty(),
        "and nothing was filed",
    );
}

#[tokio::test]
async fn a_read_only_subscriber_may_not_write_into_the_mission() {
    let harness = stream_support::Harness::start_with_missions().await;
    let writer = harness
        .enroll(
            "writer",
            WRITER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;
    let reader = harness
        .enroll(
            "reader",
            READER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;

    let mission = data_sync(
        &harness,
        &[
            (WRITER_UID, "MISSION_READONLY_SUBSCRIBER"),
            (READER_UID, "MISSION_SUBSCRIBER"),
        ],
    )
    .await;

    let mut writing = harness.eud(&writer, "WRITER").await;
    let mut reading = harness.eud(&reader, "READER").await;
    harness.await_connected(2).await;
    announce(&mut [&mut writing, &mut reading]).await;

    writing.send(addressed("UID-MARKER")).await.unwrap();

    assert_eq!(
        first_after(&mut writing, "UID-BARRIER-READONLY", &mut reading)
            .await
            .uid,
        "UID-BARRIER-READONLY",
        "a role that may only read is not a role that may publish",
    );
    assert!(
        harness
            .context
            .db()
            .mission_contents()
            .uids(mission)
            .await
            .unwrap()
            .is_empty(),
    );
}

#[tokio::test]
async fn a_subscriber_in_another_channel_gets_the_notice_and_not_the_message() {
    // R-02 H3. `compat/missions.md` §11 item 2: the raw relay to a mission's
    // subscribers is "still subject to the normal `IN`/`OUT` reachability
    // check". It used to be a bare uid-index lookup with no channel filter, so a
    // subscriber with no channel overlap with the sender received the position
    // and chat traffic the channel model says it must not see.
    //
    // §12 is the other half and is deliberately different: the `t-x-m-*`
    // notification **does** bypass the broker, so the subscriber still learns
    // that the mission changed. It just does not get the content.
    //
    // Every other case in this file puts all three parties in `blue`, which is
    // why the divergent case never ran.
    let harness = stream_support::Harness::start_with_missions().await;
    let writer = harness
        .enroll(
            "writer",
            WRITER_UID,
            &[("blue", rustak_api::identity::Direction::Both)],
        )
        .await;
    let reader = harness
        .enroll(
            "reader",
            READER_UID,
            &[("red", rustak_api::identity::Direction::Both)],
        )
        .await;

    data_sync(
        &harness,
        &[
            (WRITER_UID, "MISSION_SUBSCRIBER"),
            (READER_UID, "MISSION_SUBSCRIBER"),
        ],
    )
    .await;

    let mut writing = harness.eud(&writer, "WRITER").await;
    let mut reading = harness.eud(&reader, "READER").await;
    harness.await_connected(2).await;

    // Not `announce`: its barrier is an ordinary broadcast and these two cannot
    // reach each other, which is the whole point of the case. Each client still
    // has to say what it calls itself, because a mission notice is addressed by
    // uid and the hub only indexes a connection once it has.
    writing.send_sa(51.5, -0.12).await.expect("an SA message");
    reading.send_sa(51.5, -0.12).await.expect("an SA message");
    harness.await_callsign("WRITER").await;
    harness.await_callsign("READER").await;

    // Two writes, so that the second notice is the barrier for the first
    // message: one connection is written in order, so if the raw CoT for marker
    // one were relayed it would arrive before the notice for marker two.
    writing.send(addressed("UID-MARKER-1")).await.unwrap();
    writing.send(addressed("UID-MARKER-2")).await.unwrap();

    for expected in 1..=2 {
        let event = reading
            .expect(
                |event| event.r#type.starts_with("t-x-m-c") || event.uid.starts_with("UID-MARKER"),
                stream_support::EXPECT,
            )
            .await
            .expect("a subscriber in another channel is still told the mission changed");

        assert!(
            event.r#type.starts_with("t-x-m-c"),
            "the raw CoT reached a subscriber the sender cannot reach: {event:?}",
        );
        assert_eq!(
            event
                .detail
                .find("mission")
                .and_then(|mission| mission.get("name")),
            Some("Kettle"),
            "notice {expected}",
        );
    }
}
