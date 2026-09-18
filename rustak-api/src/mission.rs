//! Data Sync missions, as the admin API talks about them.
//!
//! These are **not** the Marti wire shapes. `/Marti/api/missions/*` speaks
//! TAK's JSON — `passwordProtected`, `missionChanges`, `mission_layers` and the
//! rest — and those shapes live in the server beside the routes that have to
//! emit them byte for byte. What is here is the administrator's view: what
//! missions exist, who is subscribed to each, what has changed in one, and the
//! two operations an operator performs that a TAK client cannot (taking a
//! subscription away, and deleting a mission somebody else owns).
//!
//! # Why the role names are the TAK spellings
//!
//! `MISSION_OWNER` and its two siblings are the strings a mission token carries
//! and a client compares against. Renaming them for the admin API would mean a
//! translation table nobody could read a bug report against, so the serialised
//! form is TAK's and only the Rust identifiers are ours.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{GroupName, MissionGuid};

/// What a subscription is allowed to do with a mission.
///
/// The three roles TAK defines, in decreasing order of what they permit. The
/// serialised form is the wire spelling, which is also what is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MissionRoleKind {
    /// Everything, including deleting the mission and handing out roles.
    #[serde(rename = "MISSION_OWNER")]
    Owner,
    /// Read and write, which is what a subscriber gets by default.
    #[serde(rename = "MISSION_SUBSCRIBER")]
    #[default]
    Subscriber,
    /// Read only.
    #[serde(rename = "MISSION_READONLY_SUBSCRIBER")]
    ReadonlySubscriber,
}

impl MissionRoleKind {
    /// Every role, most privileged first.
    pub const ALL: &'static [Self] = &[Self::Owner, Self::Subscriber, Self::ReadonlySubscriber];

    /// The wire spelling, which is also the stored one.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "MISSION_OWNER",
            Self::Subscriber => "MISSION_SUBSCRIBER",
            Self::ReadonlySubscriber => "MISSION_READONLY_SUBSCRIBER",
        }
    }

    /// Reads a role back from storage or from a request.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|it| it.as_str() == value)
    }

    /// Whether this role may change a mission's contents.
    pub fn can_write(&self) -> bool {
        matches!(self, Self::Owner | Self::Subscriber)
    }
}

/// One mission, as the admin list shows it.
///
/// Unlike `GET /Marti/api/missions`, this listing hides nothing: an invite-only
/// or password-protected mission still appears, because an operator asking what
/// is on their server is asking about all of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionSummary {
    pub guid: MissionGuid,
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// TAK's `tool`, which defaults to `public` and which the mission list
    /// filters on.
    pub tool: String,

    /// The uid of the client that created it, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,

    pub create_time: DateTime<Utc>,

    /// The channels whose members may see it.
    #[serde(default)]
    pub groups: Vec<GroupName>,

    #[serde(default)]
    pub keywords: Vec<String>,

    pub subscriber_count: u32,
    pub uid_count: u32,
    pub content_count: u32,

    /// Whether a password is needed to subscribe. The hash itself is never in
    /// this crate — see the crate documentation.
    pub password_protected: bool,

    pub invite_only: bool,

    pub default_role: MissionRoleKind,

    /// When the mission stops being served, if ever. TAK's `-1` is `None` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<DateTime<Utc>>,

    /// Set once a mission has been soft-deleted, which is what makes its Marti
    /// routes answer `410` rather than `404`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// One subscription to a mission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionSubscriptionSummary {
    pub client_uid: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    pub role: MissionRoleKind,
    pub create_time: DateTime<Utc>,

    /// Whether the subscriber is connected to the stream listener right now,
    /// which is what decides who a `t-x-m-c` actually reaches.
    #[serde(default)]
    pub connected: bool,
}

/// One node of a mission's layer tree.
///
/// Flat rather than nested: the tree is rebuilt from `parent_uid` by whoever
/// renders it, which keeps this type free of the recursion that a `Vec<Self>`
/// field would put into every round-trip test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionLayerSummary {
    pub uid: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// `GROUP`, `UID`, `CONTENTS`, `MAPLAYER` or `ITEM`, carried through as TAK
    /// spells it.
    pub kind: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uid: Option<String>,

    /// Where it sits among its siblings.
    #[serde(default)]
    pub position: i64,

    /// How many uids and resources are filed under it.
    #[serde(default)]
    pub item_count: u32,
}

/// One mission in full, for the detail page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionDetail {
    #[serde(flatten)]
    pub summary: MissionSummary,

    #[serde(default)]
    pub subscriptions: Vec<MissionSubscriptionSummary>,

    #[serde(default)]
    pub layers: Vec<MissionLayerSummary>,

    /// How many change rows the mission has, which is what the detail page
    /// offers to page through.
    #[serde(default)]
    pub change_count: u32,
}

/// What kind of change a `MissionChangeSummary` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionChangeKind {
    #[serde(rename = "CREATE_MISSION")]
    CreateMission,
    #[serde(rename = "DELETE_MISSION")]
    DeleteMission,
    #[serde(rename = "ADD_CONTENT")]
    AddContent,
    #[serde(rename = "REMOVE_CONTENT")]
    RemoveContent,
    #[serde(rename = "CREATE_DATA_FEED")]
    CreateDataFeed,
    #[serde(rename = "DELETE_DATA_FEED")]
    DeleteDataFeed,
}

impl MissionChangeKind {
    /// Every kind, in the order the `mission_changes` check constraint lists
    /// them.
    pub const ALL: &'static [Self] = &[
        Self::CreateMission,
        Self::DeleteMission,
        Self::AddContent,
        Self::RemoveContent,
        Self::CreateDataFeed,
        Self::DeleteDataFeed,
    ];

    /// The stored and wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateMission => "CREATE_MISSION",
            Self::DeleteMission => "DELETE_MISSION",
            Self::AddContent => "ADD_CONTENT",
            Self::RemoveContent => "REMOVE_CONTENT",
            Self::CreateDataFeed => "CREATE_DATA_FEED",
            Self::DeleteDataFeed => "DELETE_DATA_FEED",
        }
    }

    /// Reads a kind back from storage.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|it| it.as_str() == value)
    }
}

/// The cached description of a map item a change refers to.
///
/// Cached at the moment the item was filed, so that a change list still says
/// what was added after the item itself has moved on or gone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UidDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iconset_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
}

impl UidDetails {
    /// Whether there is nothing here worth rendering.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One change to a mission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionChangeSummary {
    pub kind: MissionChangeKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_uid: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,

    pub timestamp: DateTime<Utc>,
    pub server_time: DateTime<Utc>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<UidDetails>,
}

/// The one field `PUT /api/v1/missions/{guid}/subscriptions/{uid}/role` takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionRoleUpdate {
    pub role: MissionRoleKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> MissionSummary {
        MissionSummary {
            guid: MissionGuid::parse("0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f").unwrap(),
            name: "Operation Kettle".to_string(),
            description: None,
            tool: "public".to_string(),
            creator_uid: Some("ANDROID-1".to_string()),
            create_time: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            groups: vec![GroupName::parse("Blue").unwrap()],
            keywords: vec!["recon".to_string()],
            subscriber_count: 2,
            uid_count: 5,
            content_count: 1,
            password_protected: false,
            invite_only: false,
            default_role: MissionRoleKind::Subscriber,
            expiration: None,
            deleted_at: None,
        }
    }

    #[test]
    fn a_role_round_trips_through_the_wire_spelling() {
        for role in MissionRoleKind::ALL {
            assert_eq!(MissionRoleKind::parse(role.as_str()), Some(*role));
            assert_eq!(
                serde_json::to_string(role).unwrap(),
                format!("\"{}\"", role.as_str()),
                "the serialised form is TAK's, not ours",
            );
        }

        assert_eq!(MissionRoleKind::parse("MISSION_ADMIN"), None);
        assert!(MissionRoleKind::Owner.can_write());
        assert!(MissionRoleKind::Subscriber.can_write());
        assert!(!MissionRoleKind::ReadonlySubscriber.can_write());
    }

    #[test]
    fn a_change_kind_round_trips_through_the_stored_spelling() {
        for kind in MissionChangeKind::ALL {
            assert_eq!(MissionChangeKind::parse(kind.as_str()), Some(*kind));
            assert_eq!(
                serde_json::to_string(kind).unwrap(),
                format!("\"{}\"", kind.as_str()),
            );
        }

        assert_eq!(MissionChangeKind::parse("ADD"), None);
    }

    #[test]
    fn a_summary_round_trips_and_omits_what_is_absent() {
        let mission = summary();
        let json = serde_json::to_string(&mission).unwrap();

        assert!(!json.contains("description"), "an absent field is omitted");
        assert!(!json.contains("expiration"));
        assert!(json.contains("\"password_protected\":false"), "{json}");
        assert_eq!(
            serde_json::from_str::<MissionSummary>(&json).unwrap(),
            mission
        );
    }

    #[test]
    fn a_detail_flattens_its_summary_rather_than_nesting_it() {
        let detail = MissionDetail {
            summary: summary(),
            subscriptions: vec![MissionSubscriptionSummary {
                client_uid: "ANDROID-1".to_string(),
                username: Some("alice".to_string()),
                role: MissionRoleKind::Owner,
                create_time: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                connected: true,
            }],
            layers: vec![MissionLayerSummary {
                uid: "layer-1".to_string(),
                name: Some("Markers".to_string()),
                kind: "UID".to_string(),
                parent_uid: None,
                position: 0,
                item_count: 3,
            }],
            change_count: 9,
        };

        let json = serde_json::to_value(&detail).unwrap();

        assert_eq!(
            json.get("name").and_then(serde_json::Value::as_str),
            Some("Operation Kettle"),
            "the summary's fields sit beside the detail's, not under a key",
        );
        assert_eq!(
            serde_json::from_value::<MissionDetail>(json).unwrap(),
            detail
        );
    }

    #[test]
    fn a_change_round_trips_with_and_without_cached_details() {
        let change = MissionChangeSummary {
            kind: MissionChangeKind::AddContent,
            content_uid: Some("UID-A".to_string()),
            content_hash: None,
            timestamp: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            server_time: DateTime::from_timestamp(1_700_000_001, 0).unwrap(),
            creator_uid: Some("ANDROID-1".to_string()),
            details: Some(UidDetails {
                kind: Some("a-f-G".to_string()),
                callsign: Some("ALPHA".to_string()),
                lat: Some(51.5),
                lon: Some(-0.12),
                ..UidDetails::default()
            }),
        };

        let json = serde_json::to_string(&change).unwrap();
        assert!(!json.contains("content_hash"));
        assert_eq!(
            serde_json::from_str::<MissionChangeSummary>(&json).unwrap(),
            change
        );

        let bare = MissionChangeSummary {
            details: None,
            ..change
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("details"));
        assert_eq!(
            serde_json::from_str::<MissionChangeSummary>(&json).unwrap(),
            bare
        );
        assert!(UidDetails::default().is_empty());
    }

    #[test]
    fn a_role_update_is_the_one_field_the_endpoint_takes() {
        let update: MissionRoleUpdate =
            serde_json::from_value(serde_json::json!({ "role": "MISSION_READONLY_SUBSCRIBER" }))
                .unwrap();

        assert_eq!(update.role, MissionRoleKind::ReadonlySubscriber);
    }
}
