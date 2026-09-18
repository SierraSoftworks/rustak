//! Data packages and the other files Enterprise Sync holds, as the admin API
//! talks about them.
//!
//! These are **not** the Marti wire shapes. `/Marti/sync/search` answers a
//! Title-case `Metadata` object and `/Marti/api/sync/search` a lowerCamelCase
//! `Resource`; both are TAK's, both live in the server beside the routes that
//! emit them, and both carry `size` as a different JSON type. What is here is
//! the administrator's view of the same row: what a package is, who put it
//! there, which channels may see it, and the one flag no TAK client knows
//! about — whether it ships with an enrolment.
//!
//! # An expiry is an instant here and epoch milliseconds on the Marti surface
//!
//! TAK stores it as epoch milliseconds with `-1` meaning "never", every client
//! that reads it reads that spelling, and the column holds exactly those bits.
//! `/Marti/sync/*` therefore keeps the number, byte for byte — that contract is
//! not ours to change.
//!
//! `/api/v1` is ours, and a browser that has to know "negative means never"
//! before it can render a date is a convention travelling in a comment rather
//! than in the type. So the admin API carries an RFC 3339 instant with `null`
//! for "never", and [`PackageUpdate::expiration`] distinguishes *absent* (leave
//! the expiry alone) from *`null`* (clear it) — which the integer spelling
//! could only do by reserving a sign.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// One stored file, as the package list shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageSummary {
    /// The SHA-256 of the stored bytes, lower-case hexadecimal, and the key
    /// every other route in this area takes.
    pub hash: String,

    /// How a TAK client addresses this resource.
    pub uid: String,

    /// What it is called in a browser.
    pub name: String,

    /// The name the file arrived under, when the upload carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,

    pub mime_type: String,

    pub size: i64,

    /// The account that uploaded it, when it was uploaded by one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter: Option<String>,

    /// The `clientUid` of the device that created it, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,

    pub submission_time: DateTime<Utc>,

    #[serde(default)]
    pub keywords: Vec<String>,

    /// The channels that may see it. Empty means the default channel, which
    /// every principal holds.
    #[serde(default)]
    pub groups: Vec<String>,

    /// TAK's `tool`, which decides which client surface lists it.
    pub tool: String,

    /// When the package stops being served, or `null` for one that never
    /// does. Always present, so that "never" is stated rather than inferred
    /// from a missing key.
    #[serde(default)]
    pub expiration: Option<DateTime<Utc>>,

    /// Whether this package is delivered with an enrolment profile.
    pub install_on_enrollment: bool,

    /// Set when the package carries the `missionpackage` keyword, which is
    /// what puts it in a client's data-package browser.
    pub mission_package: bool,

    /// The Data Sync mission this resource was attached to, when it was
    /// uploaded against one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_name: Option<String>,
}

/// A change to a package's metadata.
///
/// Every field is optional and an absent one is left alone. A change with no
/// fields at all is refused rather than treated as a no-op, because it is
/// almost always a client sending the wrong body.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageUpdate {
    /// The channels that may see it. An empty list makes it visible to the
    /// default channel rather than to nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_on_enrollment: Option<bool>,

    /// Absent leaves the expiry alone; `null` clears it; an instant sets it.
    ///
    /// Two levels of option because there are three answers, and a form that
    /// cannot say "clear this" would leave an expired package unreachable for
    /// good.
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration: Option<Option<DateTime<Utc>>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl PackageUpdate {
    /// Whether this change would do nothing, which the API refuses.
    pub fn is_empty(&self) -> bool {
        self.groups.is_none()
            && self.tool.is_none()
            && self.keywords.is_none()
            && self.install_on_enrollment.is_none()
            && self.expiration.is_none()
            && self.name.is_none()
    }
}

/// Reads a present `null` as [`Some(None)`] rather than as an absent field.
///
/// `Option`'s own `Deserialize` folds both into `None`, which is what makes an
/// undecorated `Option<Option<T>>` unable to tell "leave it alone" from "clear
/// it". Reached only for a key that is actually in the body, because
/// `#[serde(default)]` answers for the ones that are not.
fn explicit_null<'de, D>(deserializer: D) -> Result<Option<Option<DateTime<Utc>>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<DateTime<Utc>>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> PackageSummary {
        PackageSummary {
            hash: "8f".repeat(32),
            uid: "3f6c1c0e-0f0a-4a1a-9f3b-000000000001".to_string(),
            name: "patrol.zip".to_string(),
            filename: Some("patrol.zip".to_string()),
            mime_type: "application/x-zip-compressed".to_string(),
            size: 4_194_304,
            submitter: Some("grace".to_string()),
            creator_uid: Some("ANDROID-1".to_string()),
            submission_time: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            keywords: vec!["missionpackage".to_string()],
            groups: vec!["Blue".to_string()],
            tool: "public".to_string(),
            expiration: Some("2026-09-25T12:00:00Z".parse().unwrap()),
            install_on_enrollment: true,
            mission_package: true,
            mission_name: None,
        }
    }

    #[test]
    fn a_package_round_trips_through_serde() {
        let json = serde_json::to_string(&summary()).unwrap();

        assert_eq!(
            serde_json::from_str::<PackageSummary>(&json).unwrap(),
            summary()
        );
    }

    #[test]
    fn never_is_stated_as_null_rather_than_left_out() {
        // The difference from the fields beside it is deliberate: an absent
        // `filename` means "we were not told", while an absent expiry would be
        // indistinguishable from an older server that did not send one.
        let json = serde_json::to_value(PackageSummary {
            expiration: None,
            filename: None,
            submitter: None,
            creator_uid: None,
            ..summary()
        })
        .unwrap();

        assert_eq!(json["expiration"], serde_json::Value::Null);
        assert!(json.get("filename").is_none());
        assert_eq!(json["size"], 4_194_304);
    }

    #[test]
    fn an_expiry_travels_as_an_instant_rather_than_as_a_number() {
        let json = serde_json::to_value(summary()).unwrap();

        assert_eq!(json["expiration"], "2026-09-25T12:00:00Z");
    }

    #[test]
    fn the_field_names_are_the_rust_ones() {
        // No `rename_all`: the admin API is ours rather than TAK's, and every
        // other type in this crate is snake_case on the wire.
        let json = serde_json::to_value(summary()).unwrap();

        assert!(json.get("install_on_enrollment").is_some());
        assert!(json.get("mission_package").is_some());
        assert!(json.get("submission_time").is_some());
    }

    #[test]
    fn an_update_with_no_fields_is_recognisably_empty() {
        let empty: PackageUpdate = serde_json::from_value(serde_json::json!({})).unwrap();

        assert!(empty.is_empty());
        assert!(
            !PackageUpdate {
                groups: Some(Vec::new()),
                ..PackageUpdate::default()
            }
            .is_empty(),
            "clearing the channels is a change, not an absent field",
        );
    }

    #[test]
    fn an_update_round_trips_and_says_which_of_the_three_it_means() {
        let update = PackageUpdate {
            groups: Some(vec!["Red".to_string()]),
            tool: Some("public".to_string()),
            keywords: Some(vec!["missionpackage".to_string()]),
            install_on_enrollment: Some(false),
            expiration: Some(Some("2026-12-01T00:00:00Z".parse().unwrap())),
            name: Some("renamed.zip".to_string()),
        };

        let json = serde_json::to_string(&update).unwrap();

        assert_eq!(
            serde_json::from_str::<PackageUpdate>(&json).unwrap(),
            update
        );
        assert!(!update.is_empty());
    }

    #[test]
    fn an_absent_expiry_leaves_it_alone_and_an_explicit_null_clears_it() {
        // The whole reason for the second `Option`: TAK's `-1` said this with a
        // sign, and nothing in a JSON body can.
        let untouched: PackageUpdate = serde_json::from_str(r#"{"name":"a.zip"}"#).unwrap();
        assert_eq!(untouched.expiration, None);
        assert!(!untouched.is_empty());

        let cleared: PackageUpdate = serde_json::from_str(r#"{"expiration":null}"#).unwrap();
        assert_eq!(cleared.expiration, Some(None));
        assert!(
            !cleared.is_empty(),
            "clearing the expiry is a change, not an absent field",
        );

        // And a cleared expiry has to survive the round trip a form makes.
        let json = serde_json::to_string(&cleared).unwrap();
        assert_eq!(json, r#"{"expiration":null}"#);
        assert_eq!(
            serde_json::from_str::<PackageUpdate>(&json)
                .unwrap()
                .expiration,
            Some(None),
        );
    }
}
