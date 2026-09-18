//! The `t-x-m-*` notices a mission pushes at the people watching it.
//!
//! A Data Sync change reaches a client twice and by two different routes: the
//! raw CoT is relayed to the mission's subscribers as an explicit-uid hit
//! ([`mission_hook`](super::mission_hook)), and *separately* a notice describing
//! the change is pushed at them. This module is the second half — the model of
//! what a notice says, and the rendering of it into the exact event template
//! `compat/missions.md` §12 fixes.
//!
//! # The mixed convention is not a mistake
//!
//! `<mission>` carries **attributes**; everything under `<MissionChanges>`
//! carries **child elements**; and `<details>` inside a `MissionChange` goes
//! back to attributes. That is what TAK Server's JAXB bindings produce and what
//! CloudTAK's parser expects, so it is reproduced element for element rather
//! than tidied.
//!
//! # One change per notice
//!
//! CloudTAK's live-update path reads `missionChanges[0].contentResource.name`
//! when `missionChanges.length === 1`, and shows nothing at all when the array
//! is longer. So a notice carrying several changes renders as several events,
//! one change each, rather than one event listing them.

use rustak_cot::detail::{MissionDetail, MissionNotice as NoticeKind, TypedDetail};
use rustak_cot::event::Point;
use rustak_cot::types::{cot_type, how};
use rustak_cot::{CotTime, Event, msgs};

use super::mission_payload::{MissionChangeXml, MissionLayerXml, MissionRoleXml};
use super::notify::Notifier;
use crate::prelude::*;

/// `t-x-m-c-k` — the mission's own keywords changed.
pub const MISSION_KEYWORD_CHANGE: &str = "t-x-m-c-k";
/// `t-x-m-c-k-u` — a filed uid's keywords changed.
pub const MISSION_UID_KEYWORD_CHANGE: &str = "t-x-m-c-k-u";
/// `t-x-m-c-k-c` — a filed resource's keywords changed.
pub const MISSION_RESOURCE_KEYWORD_CHANGE: &str = "t-x-m-c-k-c";
/// `t-x-m-c-m` — the mission's metadata, password or groups changed.
pub const MISSION_METADATA_CHANGE: &str = "t-x-m-c-m";
/// `t-x-m-c-e` — external data attached to the mission changed.
pub const MISSION_EXTERNAL_DATA_CHANGE: &str = "t-x-m-c-e";
/// `t-x-m-c-h` — the mission's layer tree changed.
pub const MISSION_LAYER_CHANGE: &str = "t-x-m-c-h";

/// Which flavour of `t-x-m-c…` a change notice is.
///
/// Every one of them carries `mission/@type="CHANGE"`; only the event type
/// differs, which is how a client tells a content add from a keyword edit
/// without parsing the payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChangeKind {
    /// Content added or removed, and the fall-through for anything unmodelled
    /// (data feeds and map layers both land here in TAK Server too).
    #[default]
    Content,
    /// A log entry was written.
    Log,
    /// The mission's own keywords.
    Keyword,
    /// A filed uid's keywords.
    UidKeyword,
    /// A filed resource's keywords.
    ResourceKeyword,
    /// Description, password, groups, default role — anything on the mission
    /// itself.
    Metadata,
    /// External data attached to the mission.
    ExternalData,
    /// The layer tree.
    Layer,
}

impl ChangeKind {
    /// The `event/@type` this kind renders as.
    pub fn cot_type(&self) -> &'static str {
        match self {
            Self::Content => cot_type::MISSION_CHANGE,
            Self::Log => cot_type::MISSION_LOG_CHANGE,
            Self::Keyword => MISSION_KEYWORD_CHANGE,
            Self::UidKeyword => MISSION_UID_KEYWORD_CHANGE,
            Self::ResourceKeyword => MISSION_RESOURCE_KEYWORD_CHANGE,
            Self::Metadata => MISSION_METADATA_CHANGE,
            Self::ExternalData => MISSION_EXTERNAL_DATA_CHANGE,
            Self::Layer => MISSION_LAYER_CHANGE,
        }
    }
}

/// The mission a notice names, as the notice needs it.
///
/// Carried rather than looked up, because a notice is rendered after the write
/// that caused it and the mission may have been deleted in between — a delete
/// notice for a mission nobody can read any more still has to name it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoticeMission {
    /// The name `<dest mission=…>` addresses it by.
    pub name: String,
    /// Its immutable identifier.
    pub guid: String,
    /// `public`, unless somebody set otherwise. Emitted even when empty.
    pub tool: String,
    /// The channels whose readers a broadcast reaches.
    pub groups: Vec<GroupName>,
}

/// Who a notice is delivered to.
///
/// Neither arm goes through the broker: a mission notice is addressed, carries
/// no flow tag, and is not subject to the sender/receiver reachability rule
/// (`compat/missions.md` §12). The broadcast arm applies its own group check
/// instead, which is a different question — "may this person read this
/// mission", not "may the author reach this person".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recipients {
    /// Exactly these client uids, whichever of them are connected.
    Subscribers(Vec<String>),
    /// Everyone who receives from any of these channels, minus the author.
    BroadcastGroups(Vec<GroupName>),
}

/// Something that happened to a mission, before it is a CoT event.
#[derive(Clone, Debug, PartialEq)]
pub enum MissionNotice {
    /// Something inside the mission changed.
    Change {
        /// Which `t-x-m-c…` it is.
        kind: ChangeKind,
        /// The mission it happened to.
        mission: NoticeMission,
        /// Who did it, and who is therefore not told.
        author_uid: Option<String>,
        /// One event is rendered per entry; an empty list renders one event
        /// with no `<MissionChanges>` at all.
        changes: Vec<MissionChangeXml>,
        /// The layer a `t-x-m-c-h` is about.
        layer: Option<MissionLayerXml>,
        /// Who hears about it.
        recipients: Recipients,
    },
    /// A mission was created: `t-x-m-n`, broadcast to its channels.
    Created {
        /// The new mission.
        mission: NoticeMission,
        /// Its creator, who is not told.
        author_uid: Option<String>,
    },
    /// A mission was deleted: `t-x-m-d`, broadcast to its channels.
    Deleted {
        /// The mission that has gone.
        mission: NoticeMission,
        /// Whoever deleted it, who is not told.
        author_uid: Option<String>,
    },
    /// Somebody was invited: `t-x-m-i`, to the invited uids only.
    Invite {
        /// The mission they were invited to.
        mission: NoticeMission,
        /// Who invited them.
        author_uid: Option<String>,
        /// The invitation token, which is what makes the invite actionable.
        token: String,
        /// The role the invitation carries.
        role: MissionRoleXml,
        /// The uids to tell.
        uids: Vec<String>,
    },
    /// A subscription's role changed: `t-x-m-r`, to the one uid.
    ///
    /// `mission/@type` is `INVITE` here, not `ROLE` — verified deliberate in
    /// TAK Server, and reproduced rather than corrected.
    RoleChange {
        /// The mission.
        mission: NoticeMission,
        /// Who changed it.
        author_uid: Option<String>,
        /// The new role.
        role: MissionRoleXml,
        /// Whose role it is.
        uid: String,
    },
}

impl MissionNotice {
    /// The mission this notice is about.
    pub fn mission(&self) -> &NoticeMission {
        match self {
            Self::Change { mission, .. }
            | Self::Created { mission, .. }
            | Self::Deleted { mission, .. }
            | Self::Invite { mission, .. }
            | Self::RoleChange { mission, .. } => mission,
        }
    }

    /// Who caused it, and is therefore never told about it.
    pub fn author_uid(&self) -> Option<&str> {
        match self {
            Self::Change { author_uid, .. }
            | Self::Created { author_uid, .. }
            | Self::Deleted { author_uid, .. }
            | Self::Invite { author_uid, .. }
            | Self::RoleChange { author_uid, .. } => author_uid.as_deref(),
        }
    }

    /// Who this notice goes to.
    pub fn recipients(&self) -> Recipients {
        match self {
            Self::Change { recipients, .. } => recipients.clone(),
            Self::Created { mission, .. } | Self::Deleted { mission, .. } => {
                Recipients::BroadcastGroups(mission.groups.clone())
            }
            Self::Invite { uids, .. } => Recipients::Subscribers(uids.clone()),
            Self::RoleChange { uid, .. } => Recipients::Subscribers(vec![uid.clone()]),
        }
    }
}

/// Renders a notice and pushes it at whoever it is for.
///
/// Answers how many connections it reached, summed over the events it became.
/// A notice nobody is connected to hear reaches nobody and is not an error:
/// the change is already durable, and the subscriber will read it back on its
/// next `/changes` call.
pub fn send(notifier: &dyn Notifier, notice: &MissionNotice) -> usize {
    let recipients = notice.recipients();
    let author = notice.author_uid().map(ToOwned::to_owned);

    let reached: usize = events(notice, CotTime::now())
        .into_iter()
        .map(|event| match &recipients {
            Recipients::Subscribers(uids) => notifier.send_to_uids(uids, event),
            Recipients::BroadcastGroups(groups) => {
                notifier.broadcast_to_groups(groups, author.as_deref(), event)
            }
        })
        .sum();

    debug!(
        mission = notice.mission().name,
        reached, "Pushed a mission notice at the clients watching it."
    );

    reached
}

/// Renders a notice into the events it becomes, with fresh uids.
pub fn events(notice: &MissionNotice, now: CotTime) -> Vec<Event> {
    events_with(notice, now, || uuid::Uuid::new_v4().to_string())
}

/// The same, with the event uids supplied — what the golden tests pin.
pub fn events_with(
    notice: &MissionNotice,
    now: CotTime,
    mut uid: impl FnMut() -> String,
) -> Vec<Event> {
    let mission = notice.mission();

    match notice {
        MissionNotice::Change {
            kind,
            changes,
            layer,
            author_uid,
            ..
        } if !changes.is_empty() => changes
            .iter()
            .map(|change| {
                let mut detail = base(mission, NoticeKind::Change, author_uid.as_deref());
                detail.changes.push(change.to_change(mission));

                envelope(kind.cot_type(), uid(), detail, layer.as_ref(), None, now)
            })
            .collect(),
        MissionNotice::Change {
            kind,
            layer,
            author_uid,
            ..
        } => {
            let detail = base(mission, NoticeKind::Change, author_uid.as_deref());

            vec![envelope(
                kind.cot_type(),
                uid(),
                detail,
                layer.as_ref(),
                None,
                now,
            )]
        }
        MissionNotice::Created { author_uid, .. } => {
            let detail = base(mission, NoticeKind::Create, author_uid.as_deref());

            vec![envelope(
                cot_type::MISSION_CREATE,
                uid(),
                detail,
                None,
                None,
                now,
            )]
        }
        MissionNotice::Deleted { author_uid, .. } => {
            let detail = base(mission, NoticeKind::Delete, author_uid.as_deref());

            vec![envelope(
                cot_type::MISSION_DELETE,
                uid(),
                detail,
                None,
                None,
                now,
            )]
        }
        MissionNotice::Invite {
            author_uid,
            token,
            role,
            ..
        } => {
            let mut detail = base(mission, NoticeKind::Invite, author_uid.as_deref());
            detail.token = Some(token.clone());

            vec![envelope(
                cot_type::MISSION_INVITE,
                uid(),
                detail,
                None,
                Some(role),
                now,
            )]
        }
        MissionNotice::RoleChange {
            author_uid, role, ..
        } => {
            let detail = base(mission, NoticeKind::Invite, author_uid.as_deref());

            vec![envelope(
                cot_type::MISSION_ROLE_CHANGE,
                uid(),
                detail,
                None,
                Some(role),
                now,
            )]
        }
    }
}

/// The `<mission>` attributes every notice carries.
fn base(mission: &NoticeMission, kind: NoticeKind, author_uid: Option<&str>) -> MissionDetail {
    MissionDetail {
        r#type: kind,
        tool: Some(mission.tool.clone()),
        name: Some(mission.name.clone()),
        guid: Some(mission.guid.clone()),
        author_uid: author_uid.map(ToOwned::to_owned),
        ..MissionDetail::default()
    }
}

/// The shared event: null island, `h-g-i-g-o`, and a twenty-second stale.
fn envelope(
    r#type: &str,
    uid: String,
    detail: MissionDetail,
    layer: Option<&MissionLayerXml>,
    role: Option<&MissionRoleXml>,
    now: CotTime,
) -> Event {
    let mut mission = detail.to_element();

    if let Some(layer) = layer {
        mission.push(layer.to_element());
    }
    if let Some(role) = role {
        mission.push(role.to_element());
    }

    Event::builder(r#type, uid)
        .how(how::H_G_I_G_O)
        .point_full(Point::zero())
        .time(now)
        .stale_after(msgs::NOTICE_VALIDITY)
        .push(mission)
        .build()
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use rustak_api::MissionRoleKind;

    use super::super::mission_payload::{ResourceXml, UidDetailsXml};
    use super::*;

    /// `2026-09-17T12:00:40.000Z`, the instant the golden fixtures use.
    const NOW: CotTime = CotTime::from_millis(1_789_646_440_000);

    fn mission() -> NoticeMission {
        NoticeMission {
            name: "Operation Kettle".into(),
            guid: "0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f".into(),
            tool: "public".into(),
            groups: vec![GroupName::parse("Blue").unwrap()],
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_789_646_440_000).unwrap()
    }

    pub(crate) fn rendered(event: &Event) -> String {
        String::from_utf8(rustak_cot::xml::write(event).to_vec()).expect("the writer emits UTF-8")
    }

    fn one(notice: &MissionNotice) -> Event {
        let mut next = 0;
        let mut events = events_with(notice, NOW, || {
            next += 1;
            format!("notice-{next}")
        });

        assert_eq!(events.len(), 1, "one event was expected");

        events.remove(0)
    }

    #[test]
    fn every_change_kind_has_its_own_event_type() {
        for (kind, expected) in [
            (ChangeKind::Content, "t-x-m-c"),
            (ChangeKind::Log, "t-x-m-c-l"),
            (ChangeKind::Keyword, "t-x-m-c-k"),
            (ChangeKind::UidKeyword, "t-x-m-c-k-u"),
            (ChangeKind::ResourceKeyword, "t-x-m-c-k-c"),
            (ChangeKind::Metadata, "t-x-m-c-m"),
            (ChangeKind::ExternalData, "t-x-m-c-e"),
            (ChangeKind::Layer, "t-x-m-c-h"),
        ] {
            assert_eq!(kind.cot_type(), expected);
        }
    }

    #[test]
    fn a_notice_is_a_null_island_event_that_goes_stale_in_twenty_seconds() {
        let event = one(&MissionNotice::Created {
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
        });

        assert_eq!(event.r#type, "t-x-m-n");
        assert_eq!(event.how.as_deref(), Some("h-g-i-g-o"));
        assert_eq!(event.stale - event.time, 20_000);
        assert_eq!(event.point.lat, 0.0);
        assert_eq!(event.point.ce, 9_999_999.0);
        let detail = event.detail.find("mission").expect("a <mission> child");
        assert_eq!(detail.get("type"), Some("CREATE"));
        assert_eq!(detail.get("authorUid"), Some("ANDROID-1"));
        assert_eq!(detail.get("tool"), Some("public"));
    }

    #[test]
    fn several_changes_become_several_events_with_one_change_each() {
        // CloudTAK reads missionChanges[0] only when the array has one entry,
        // so batching them into one notice would show the operator nothing.
        let notice = MissionNotice::Change {
            kind: ChangeKind::Content,
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
            changes: vec![
                MissionChangeXml::new("ADD_CONTENT", at()),
                MissionChangeXml::new("ADD_CONTENT", at()),
            ],
            layer: None,
            recipients: Recipients::Subscribers(vec!["ANDROID-2".into()]),
        };

        let mut next = 0;
        let events = events_with(&notice, NOW, || {
            next += 1;
            format!("notice-{next}")
        });

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].uid, "notice-1");
        assert_eq!(events[1].uid, "notice-2");
        for event in &events {
            let changes = event
                .detail
                .find("mission")
                .and_then(|mission| mission.child("MissionChanges"))
                .expect("a <MissionChanges> child");

            assert_eq!(changes.elements().count(), 1);
        }
    }

    #[test]
    fn a_change_with_nothing_to_list_still_renders_one_event() {
        let event = one(&MissionNotice::Change {
            kind: ChangeKind::Keyword,
            mission: mission(),
            author_uid: None,
            changes: Vec::new(),
            layer: None,
            recipients: Recipients::BroadcastGroups(vec![GroupName::parse("Blue").unwrap()]),
        });

        let detail = event.detail.find("mission").unwrap();
        assert_eq!(event.r#type, "t-x-m-c-k");
        assert!(detail.child("MissionChanges").is_none());
        assert_eq!(detail.get("authorUid"), None, "an absent author is omitted");
    }

    #[test]
    fn a_role_change_says_invite_because_tak_server_does() {
        let event = one(&MissionNotice::RoleChange {
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
            role: MissionRoleXml(MissionRoleKind::ReadonlySubscriber),
            uid: "ANDROID-2".into(),
        });

        let detail = event.detail.find("mission").unwrap();
        assert_eq!(event.r#type, "t-x-m-r");
        assert_eq!(
            detail.get("type"),
            Some("INVITE"),
            "not ROLE — reproduced deliberately",
        );
        let role = detail.child("role").expect("a <role> child");
        assert_eq!(role.get("type"), Some("MISSION_READONLY_SUBSCRIBER"));

        // Repeated `<permissions>` text elements, which is what JAXB renders
        // from `@XmlElement(name="permissions")` on a `Set<String>` — research
        // 05 §7.5, and R-02 M11 for why the nested shape was wrong.
        let permissions: Vec<String> = role
            .elements()
            .filter(|child| child.name == "permissions")
            .map(rustak_cot::detail::Element::text)
            .collect();
        assert_eq!(permissions, vec!["MISSION_READ".to_string()]);
    }

    #[test]
    fn an_invite_carries_its_token_and_the_role_it_grants() {
        let event = one(&MissionNotice::Invite {
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
            token: "eyJ0.invite".into(),
            role: MissionRoleXml(MissionRoleKind::Owner),
            uids: vec!["ANDROID-2".into()],
        });

        let detail = event.detail.find("mission").unwrap();
        assert_eq!(event.r#type, "t-x-m-i");
        assert_eq!(detail.get("type"), Some("INVITE"));
        assert_eq!(detail.get("token"), Some("eyJ0.invite"));
        let permissions: Vec<String> = detail
            .child("role")
            .unwrap()
            .elements()
            .filter(|child| child.name == "permissions")
            .map(rustak_cot::detail::Element::text)
            .collect();
        assert_eq!(
            permissions.len(),
            8,
            "an owner holds all eight permissions, one <permissions> element each",
        );
        assert_eq!(permissions[0], "MISSION_READ");
    }

    #[test]
    fn a_content_add_carries_its_resource_as_child_elements() {
        // CloudTAK's add/remove UI reads contentResource/name and /filename;
        // rendering them as attributes would leave it showing nothing.
        let mut change = MissionChangeXml::new("ADD_CONTENT", at());
        change.creator_uid = Some("ANDROID-1".into());
        change.resource = Some(ResourceXml {
            creator_uid: "ANDROID-1".into(),
            expiration: -1,
            filename: "plan.pdf".into(),
            hash: "abc123".into(),
            keywords: vec!["missionpackage".into()],
            mime_type: "application/pdf".into(),
            name: "Plan".into(),
            size: 42,
            submission_time: at(),
            submitter: "alice".into(),
            tool: "public".into(),
            uid: "RES-1".into(),
        });

        let event = one(&MissionNotice::Change {
            kind: ChangeKind::Content,
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
            changes: vec![change],
            layer: None,
            recipients: Recipients::Subscribers(vec!["ANDROID-2".into()]),
        });

        let entry = event
            .detail
            .find("mission")
            .and_then(|mission| mission.child("MissionChanges"))
            .and_then(|changes| changes.child("MissionChange"))
            .expect("a <MissionChange>");

        assert!(entry.attrs.is_empty(), "every field is a child element");
        assert_eq!(entry.child("type").unwrap().text(), "ADD_CONTENT");
        assert_eq!(entry.child("isFederatedChange").unwrap().text(), "false");
        let resource = entry.child("contentResource").expect("<contentResource>");
        assert!(resource.attrs.is_empty());
        assert_eq!(resource.child("name").unwrap().text(), "Plan");
        assert_eq!(resource.child("hash").unwrap().text(), "abc123");
        assert_eq!(
            resource.child("submissionTime").unwrap().text(),
            "2026-09-17T12:00:40.000Z",
        );
    }

    #[test]
    fn a_uids_details_are_attributes_with_a_location_child() {
        let mut change = MissionChangeXml::new("ADD_CONTENT", at());
        change.content_uid = Some("UID-A".into());
        change.details = Some(UidDetailsXml {
            kind: Some("a-f-G-U-C".into()),
            callsign: Some("ALPHA".into()),
            color: Some("-1".into()),
            iconset_path: Some("COT_MAPPING_2525B/a-f/a-f-G".into()),
            title: None,
            location: Some((51.5, -0.12)),
        });

        let event = one(&MissionNotice::Change {
            kind: ChangeKind::Content,
            mission: mission(),
            author_uid: None,
            changes: vec![change],
            layer: None,
            recipients: Recipients::Subscribers(vec!["ANDROID-2".into()]),
        });

        let details = event
            .detail
            .find("mission")
            .and_then(|mission| mission.child("MissionChanges"))
            .and_then(|changes| changes.child("MissionChange"))
            .and_then(|change| change.child("details"))
            .expect("a <details>");

        assert_eq!(details.get("callsign"), Some("ALPHA"));
        assert_eq!(details.get("title"), None, "an absent field is omitted");
        let location = details.child("location").expect("a <location>");
        assert_eq!(location.get("lat"), Some("51.5"));
        assert_eq!(location.get("lon"), Some("-0.12"));
    }

    #[test]
    fn a_layer_change_carries_the_layer_it_is_about() {
        let event = one(&MissionNotice::Change {
            kind: ChangeKind::Layer,
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
            changes: Vec::new(),
            layer: Some(MissionLayerXml {
                uid: "layer-1".into(),
                name: Some("Markers".into()),
                kind: "UID".into(),
                parent_uid: None,
            }),
            recipients: Recipients::Subscribers(vec!["ANDROID-2".into()]),
        });

        let layer = event
            .detail
            .find("mission")
            .and_then(|mission| mission.child("missionLayer"))
            .expect("a <missionLayer>");

        assert_eq!(event.r#type, "t-x-m-c-h");
        assert_eq!(layer.get("uid"), Some("layer-1"));
        assert_eq!(layer.get("type"), Some("UID"));
        assert_eq!(layer.get("parentUid"), None);
    }

    #[test]
    fn a_notice_knows_who_it_is_for_without_being_rendered() {
        let created = MissionNotice::Created {
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
        };

        assert_eq!(created.author_uid(), Some("ANDROID-1"));
        assert_eq!(created.mission().name, "Operation Kettle");
        assert_eq!(
            created.recipients(),
            Recipients::BroadcastGroups(vec![GroupName::parse("Blue").unwrap()]),
        );
        assert_eq!(
            MissionNotice::RoleChange {
                mission: mission(),
                author_uid: None,
                role: MissionRoleXml(MissionRoleKind::Subscriber),
                uid: "ANDROID-2".into(),
            }
            .recipients(),
            Recipients::Subscribers(vec!["ANDROID-2".into()]),
        );
    }

    #[test]
    fn the_rendered_event_matches_the_verified_template() {
        let event = one(&MissionNotice::Created {
            mission: mission(),
            author_uid: Some("ANDROID-1".into()),
        });

        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                rustak_cot::xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="notice-1" type="t-x-m-n" how="h-g-i-g-o" "#,
                    r#"time="2026-09-17T12:00:40.000Z" start="2026-09-17T12:00:40.000Z" "#,
                    r#"stale="2026-09-17T12:01:00.000Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="9999999.0" le="9999999.0"/>"#,
                    r#"<detail><mission name="Operation Kettle" "#,
                    r#"guid="0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f" type="CREATE" "#,
                    r#"authorUid="ANDROID-1" tool="public"/></detail></event>"#,
                )
            )
        );
    }
}
