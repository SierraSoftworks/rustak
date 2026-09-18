//! The exact JSON a mission endpoint emits and accepts.
//!
//! Structs only: every field name, every `skip_serializing_if` and every
//! always-present array is a compatibility decision, and keeping the rendering
//! out of this file means the decisions can be read in one place. The functions
//! that fill them live beside the data they need — [`super::service`] for a
//! mission, [`super::changes`] for a change, [`super::subscriptions`] for a
//! subscription.
//!
//! # Which fields are always present
//!
//! CloudTAK's schema makes `externalData`, `feeds`, `mapLayers`, `uids` and
//! `contents` non-optional arrays, `passwordProtected` and `inviteOnly`
//! non-optional booleans and `expiration` a non-optional number. A mission that
//! omitted any of them because it had nothing to say would fail validation
//! there, so they are emitted empty rather than skipped. `token` and
//! `ownerRole` are the opposite: they appear **only** on the `201` a create
//! answers with, and CloudTAK reads their absence as "this was an update".

use std::collections::BTreeMap;

use crate::files::ResourceJson;
use crate::prelude::*;

/// A mission, as every mission-family endpoint renders one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionJson {
    pub name: String,
    /// Always a string: CloudTAK's schema has no room for a missing one.
    pub description: String,
    #[serde(rename = "chatRoom", skip_serializing_if = "Option::is_none")]
    pub chat_room: Option<String>,
    #[serde(rename = "baseLayer", skip_serializing_if = "Option::is_none")]
    pub base_layer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<String>,
    #[serde(rename = "boundingPolygon")]
    pub bounding_polygon: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    pub tool: String,
    pub keywords: Vec<String>,
    #[serde(rename = "creatorUid", skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,
    /// Padded milliseconds, `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'`.
    #[serde(rename = "createTime")]
    pub create_time: String,
    #[serde(rename = "lastEdited", skip_serializing_if = "Option::is_none")]
    pub last_edited: Option<String>,
    /// Epoch seconds, or `-1` for a mission that never expires.
    pub expiration: i64,
    pub uids: Vec<MissionAddJson<String>>,
    pub contents: Vec<MissionAddJson<ResourceJson>>,
    pub groups: Vec<String>,
    #[serde(rename = "externalData")]
    pub external_data: Vec<serde_json::Value>,
    #[serde(rename = "mapLayers")]
    pub map_layers: Vec<serde_json::Value>,
    pub feeds: Vec<serde_json::Value>,
    #[serde(rename = "passwordProtected")]
    pub password_protected: bool,
    #[serde(rename = "inviteOnly")]
    pub invite_only: bool,
    #[serde(rename = "defaultRole")]
    pub default_role: MissionRoleJson,
    /// Only on the `201` a create answers with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Only on the `201` a create answers with.
    #[serde(rename = "ownerRole", skip_serializing_if = "Option::is_none")]
    pub owner_role: Option<MissionRoleJson>,
    /// Only when the caller asked with `?changes=true`.
    #[serde(rename = "missionChanges", skip_serializing_if = "Option::is_none")]
    pub mission_changes: Option<Vec<MissionChangeJson>>,
    /// Only when the caller asked with `?logs=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<Vec<serde_json::Value>>,
    pub guid: String,
}

/// One thing filed under a mission, and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionAddJson<T> {
    /// A map item's uid, or a whole resource.
    pub data: T,
    /// Padded milliseconds.
    pub timestamp: String,
    #[serde(rename = "creatorUid", skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,
    pub keywords: Vec<String>,
}

/// A role and the permissions it carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionRoleJson {
    #[serde(rename = "type")]
    pub kind: String,
    pub permissions: Vec<String>,
}

/// One entry of a mission's change log.
///
/// `serverTime` is camelCase here and lower-case on a log entry; the two are
/// genuinely spelled differently upstream and a client reads whichever the
/// endpoint it called emits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionChangeJson {
    #[serde(rename = "type")]
    pub kind: String,
    /// Padded milliseconds, as the client dated the change.
    pub timestamp: String,
    /// Padded milliseconds, as we recorded it.
    #[serde(rename = "serverTime")]
    pub server_time: String,
    #[serde(rename = "missionName")]
    pub mission_name: String,
    #[serde(rename = "missionGuid")]
    pub mission_guid: String,
    /// Always present; federation is not implemented, so always false.
    #[serde(rename = "isFederatedChange")]
    pub is_federated_change: bool,
    #[serde(rename = "contentUid", skip_serializing_if = "Option::is_none")]
    pub content_uid: Option<String>,
    #[serde(rename = "creatorUid", skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<UidDetailsJson>,
    #[serde(rename = "contentResource", skip_serializing_if = "Option::is_none")]
    pub content_resource: Option<ResourceJson>,
}

/// The cached rendering fields of a filed map item.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UidDetailsJson {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "iconsetPath", skip_serializing_if = "Option::is_none")]
    pub iconset_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attachments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<LocationJson>,
}

/// Where a filed map item was when it was filed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LocationJson {
    pub lat: f64,
    pub lon: f64,
}

/// One device's subscription to a mission.
///
/// `createTime` uses the **unpadded** millisecond form here, unlike
/// `Mission.createTime` — don't unify the two formatters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionSubscriptionJson {
    /// Present on the `201` a subscribe answers with, and on the singular
    /// `GET`; omitted from the plural role listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Nested only when the client said it speaks `API_VERSION` 3 or newer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission: Option<MissionJson>,
    #[serde(rename = "clientUid")]
    pub client_uid: String,
    pub username: String,
    #[serde(rename = "createTime")]
    pub create_time: String,
    pub role: MissionRoleJson,
}

/// What `PUT …/contents` was asked to file.
///
/// At least one of the three has to be non-empty; a body naming nothing is a
/// `400` rather than a silent success.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MissionContentBody {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uids: Vec<String>,
    /// Layer uid to the items filed under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<BTreeMap<String, Vec<MissionContentBody>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl MissionContentBody {
    /// Whether this body names anything to file, at any depth.
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
            && self.uids.is_empty()
            && self.paths.as_ref().is_none_or(|paths| {
                paths
                    .values()
                    .all(|nested| nested.iter().all(Self::is_empty))
            })
    }

    /// Every `(layer uid, item)` pair this body names, the top level first.
    ///
    /// Flattened rather than walked recursively by the caller, because filing
    /// is the same operation at every depth and the only thing the depth
    /// changes is which layer the item lands in.
    pub fn flatten(&self) -> Vec<(Option<String>, Filed)> {
        let mut found = Vec::new();

        self.collect_into(None, &mut found);

        found
    }

    fn collect_into(&self, layer: Option<String>, found: &mut Vec<(Option<String>, Filed)>) {
        for hash in &self.hashes {
            found.push((layer.clone(), Filed::Hash(hash.clone())));
        }

        for uid in &self.uids {
            found.push((layer.clone(), Filed::Uid(uid.clone())));
        }

        for (layer_uid, nested) in self.paths.iter().flatten() {
            for body in nested {
                body.collect_into(Some(layer_uid.clone()), found);
            }
        }
    }
}

/// One thing a contents body named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filed {
    /// A resource, by content hash.
    Hash(String),
    /// A map item, by uid.
    Uid(String),
}

/// A mission's own JSON body on a create or update.
///
/// Whatever it names overrides the matching query parameter; whatever it does
/// not name leaves the parameter alone. Unknown fields are ignored rather than
/// refused, because a newer client sending a field we have never heard of
/// should still be able to create a mission.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct MissionBody {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "chatRoom")]
    pub chat_room: Option<String>,
    #[serde(rename = "baseLayer")]
    pub base_layer: Option<String>,
    pub bbox: Option<String>,
    #[serde(rename = "boundingPolygon")]
    pub bounding_polygon: Option<Vec<String>>,
    pub path: Option<String>,
    pub classification: Option<String>,
    pub tool: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub password: Option<String>,
    #[serde(rename = "defaultRole")]
    pub default_role: Option<serde_json::Value>,
    pub expiration: Option<i64>,
    #[serde(rename = "inviteOnly")]
    pub invite_only: Option<bool>,
    pub groups: Option<Vec<String>>,
    #[serde(rename = "creatorUid")]
    pub creator_uid: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contents_body_that_names_nothing_is_empty() {
        assert!(MissionContentBody::default().is_empty());
        assert!(
            MissionContentBody {
                paths: Some(BTreeMap::from([("layer".to_string(), vec![])])),
                ..MissionContentBody::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn nested_paths_flatten_with_the_layer_they_were_filed_under() {
        let body = MissionContentBody {
            uids: vec!["top".to_string()],
            paths: Some(BTreeMap::from([(
                "layer-1".to_string(),
                vec![MissionContentBody {
                    hashes: vec!["abc".to_string()],
                    ..MissionContentBody::default()
                }],
            )])),
            ..MissionContentBody::default()
        };

        assert_eq!(
            body.flatten(),
            vec![
                (None, Filed::Uid("top".to_string())),
                (Some("layer-1".to_string()), Filed::Hash("abc".to_string())),
            ]
        );
    }

    #[test]
    fn a_role_renders_as_a_type_and_a_permission_list() {
        let json = serde_json::to_string(&MissionRoleJson {
            kind: "MISSION_SUBSCRIBER".to_string(),
            permissions: vec!["MISSION_READ".to_string()],
        })
        .unwrap();

        assert_eq!(
            json,
            r#"{"type":"MISSION_SUBSCRIBER","permissions":["MISSION_READ"]}"#
        );
    }

    #[test]
    fn a_body_ignores_fields_it_has_never_heard_of() {
        let parsed: MissionBody =
            serde_json::from_str(r#"{"description":"x","somethingNew":1}"#).unwrap();

        assert_eq!(parsed.description.as_deref(), Some("x"));
    }
}
