//! The mission payloads, as the Marti API renders them.
//!
//! Split from [`missions`](super::missions) — which is the calls — only because
//! both together would be past this repository's file-length rule. The types are
//! re-exported from [`super`], so a plugin never names this module.
//!
//! Every field defaults and unknown ones are ignored, deliberately: TAK's
//! mission payload grows between releases, and a client that refused a field it
//! had not heard of would break on a server upgrade it did not need to care
//! about. The parts TAK itself treats as opaque — a CoT detail, a role's
//! permission list — stay [`serde_json::Value`].

use rustak_core::prelude::*;

use super::files::Resource;

/// A mission, as the Marti API renders one.
///
/// Lenient by design: every field defaults, and unknown ones are ignored. TAK's
/// mission payload grows between releases and a plugin that refused a field it
/// had not heard of would break on an upgrade it did not care about.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Mission {
    pub name: String,

    #[serde(default)]
    pub guid: Option<String>,

    #[serde(default)]
    pub description: String,

    /// `public` unless somebody said otherwise; the tool a mission belongs to.
    #[serde(default)]
    pub tool: String,

    #[serde(default)]
    pub keywords: Vec<String>,

    #[serde(default)]
    pub creator_uid: Option<String>,

    /// `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'`, as a string: TAK emits two different
    /// millisecond formats across the API and parsing them into one type here
    /// would quietly normalise a difference a contract test cares about.
    #[serde(default)]
    pub create_time: Option<String>,

    /// Epoch seconds, or `-1` for a mission that does not expire.
    #[serde(default)]
    pub expiration: i64,

    /// The channels the mission belongs to.
    #[serde(default)]
    pub groups: Vec<String>,

    /// Present only on the `201` a creation answers, and on a read that spent a
    /// password.
    #[serde(default)]
    pub token: Option<String>,

    #[serde(default)]
    pub password_protected: bool,

    #[serde(default)]
    pub invite_only: bool,

    /// The CoT uids filed into the mission.
    #[serde(default)]
    pub uids: Vec<MissionItem<String>>,

    /// The files filed into it.
    #[serde(default)]
    pub contents: Vec<MissionItem<Resource>>,

    /// Filled by [`Missions::get_with_changes`](super::Missions::get_with_changes).
    #[serde(default)]
    pub mission_changes: Vec<MissionChange>,
}

/// One thing filed into a mission, and when.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MissionItem<T> {
    pub data: T,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub creator_uid: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// One entry of a mission's change log.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MissionChange {
    /// `ADD_CONTENT`, `REMOVE_CONTENT`, `CREATE_MISSION`, …
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub server_time: String,
    #[serde(default)]
    pub mission_name: String,
    #[serde(default)]
    pub content_uid: Option<String>,
    #[serde(default)]
    pub creator_uid: Option<String>,
    /// The CoT detail of a filed uid, when the change was one.
    #[serde(default)]
    pub details: Option<serde_json::Value>,
    /// The file, when the change was one.
    #[serde(default)]
    pub content_resource: Option<Resource>,
}

/// A subscription to a mission, and the token that keeps it usable.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MissionSubscription {
    /// Present on the subscription itself; hold it and pass it to
    /// [`Missions::with_token`](super::Missions::with_token).
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub client_uid: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub create_time: String,
    #[serde(default)]
    pub role: serde_json::Value,
}

/// An entry in a mission's operational log.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MissionLog {
    /// Absent when writing a new entry; assigned by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    pub content: String,

    #[serde(
        rename = "creatorUid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub creator_uid: Option<String>,

    #[serde(rename = "missionNames", default)]
    pub mission_names: Vec<String>,

    /// Assigned by the server; sending it on a write is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servertime: Option<String>,

    #[serde(rename = "contentHashes", default)]
    pub content_hashes: Vec<String>,

    #[serde(default)]
    pub keywords: Vec<String>,
}

/// What to create a mission with.
#[derive(Debug, Clone, Default)]
pub struct MissionCreate {
    /// Who is creating it, which is also who is not told about it.
    pub creator_uid: Option<String>,
    pub description: Option<String>,
    /// Defaults to `public` on the server.
    pub tool: Option<String>,
    pub keywords: Vec<String>,
    /// The channels it belongs to; empty means the caller's own.
    pub groups: Vec<String>,
    pub password: Option<String>,
}

impl MissionCreate {
    /// A mission created by a given device, with nothing else said.
    pub fn by(creator_uid: impl Into<String>) -> Self {
        Self {
            creator_uid: Some(creator_uid.into()),
            ..Self::default()
        }
    }

    /// The query parameters this becomes.
    pub(super) fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = Vec::new();

        for (key, value) in [
            ("creatorUid", &self.creator_uid),
            ("description", &self.description),
            ("tool", &self.tool),
            ("password", &self.password),
        ] {
            if let Some(value) = value {
                query.push((key, value.clone()));
            }
        }

        // Both are repeated rather than comma-joined: a keyword or a channel
        // name may itself contain a comma.
        query.extend(self.keywords.iter().map(|k| ("keyword", k.clone())));
        query.extend(self.groups.iter().map(|g| ("group", g.clone())));

        query
    }
}
