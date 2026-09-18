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
//! # Why an expiry is milliseconds rather than a timestamp
//!
//! TAK stores it as epoch milliseconds with `-1` meaning "never", every client
//! that reads it reads that spelling, and the column holds exactly those bits.
//! Rendering it as an RFC 3339 instant here would mean two representations of
//! one value and a conversion at each end for no gain, so the number travels
//! as it is stored and [`PackageUpdate::expiration`] clears it with a negative.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

    /// Epoch milliseconds, or absent for a package that does not expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<i64>,

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

    /// Epoch milliseconds. A negative value — TAK's own `-1` — clears the
    /// expiry, which is why this is not an instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<i64>,

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
            expiration: Some(1_790_000_000_000),
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
    fn an_absent_expiry_is_omitted_rather_than_null() {
        // The UI tests `package.expiration` for truthiness; a `null` and an
        // absent key read the same there, but a stored `null` would travel
        // back as an explicit "no expiry" on a round trip through a form.
        let json = serde_json::to_value(PackageSummary {
            expiration: None,
            filename: None,
            submitter: None,
            creator_uid: None,
            ..summary()
        })
        .unwrap();

        assert!(json.get("expiration").is_none());
        assert!(json.get("filename").is_none());
        assert_eq!(json["size"], 4_194_304);
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
    fn an_update_round_trips_and_carries_taks_own_never() {
        let update = PackageUpdate {
            groups: Some(vec!["Red".to_string()]),
            tool: Some("public".to_string()),
            keywords: Some(vec!["missionpackage".to_string()]),
            install_on_enrollment: Some(false),
            expiration: Some(-1),
            name: Some("renamed.zip".to_string()),
        };

        let json = serde_json::to_string(&update).unwrap();

        assert_eq!(
            serde_json::from_str::<PackageUpdate>(&json).unwrap(),
            update
        );
        assert!(!update.is_empty());
    }
}
