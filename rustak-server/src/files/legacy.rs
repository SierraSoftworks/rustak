//! The Title-case `Metadata` object, and the display map the file manager reads.
//!
//! # Why the values are strings
//!
//! TAK Server's `Metadata` holds every field as a `String[]` and serialises it
//! straight out, so `Size` and `PrimaryKey` arrive at a client as JSON
//! *strings* even though they hold numbers. CloudTAK parses them back with
//! `parseInt`; ATAK's package browser treats a `PrimaryKey` it cannot parse as
//! a non-negative integer as fatal to the **whole** response, not just to the
//! row. So the numbers are rendered as strings and the rows that could not be
//! rendered are dropped rather than emitted broken.
//!
//! Absent values are **omitted**, never `null`: the same browser reads
//! `Keywords` and indexes into it.
//!
//! `EXPIRATION` is the one upper-case key, and CloudTAK requires it to be
//! present whether or not the resource expires — `"-1"` is the "never" it
//! expects.
//!
//! # And why the file manager's map is different again
//!
//! `/Marti/api/files/metadata` is a *display* surface: its `Size` is `"12kB"`
//! rather than a byte count and its `Time` is Java's `Date.toString()`. Nothing
//! parses those two programmatically — but CloudTAK does read `Hash`, `Groups`
//! and `Time` out of the same map to fill in a package's channels column, so
//! the keys themselves are contractual.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::db::repos::ResourceRow;
use crate::marti::time;

/// What `EXPIRATION` carries when a resource does not expire.
pub const NEVER: &str = "-1";

/// What the file manager shows for a resource whose size is unknown.
const UNKNOWN_SIZE: &str = "Unknown";

/// The Title-case `Metadata` object the legacy servlets emit.
///
/// Every value is a string or an array of strings, and a key with no value is
/// left out entirely.
pub fn metadata(resource: &ResourceRow) -> Value {
    let mut object = Map::new();
    let mut put = |key: &str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            object.insert(key.to_string(), Value::String(value));
        }
    };

    put("UID", Some(resource.uid.clone()));
    put("Name", Some(resource.name.clone()));
    put("Hash", Some(resource.hash.clone()));
    put("PrimaryKey", Some(resource.id.to_string()));
    put(
        "SubmissionDateTime",
        Some(time::cot_date(resource.submission_time)),
    );
    put("SubmissionUser", resource.submitter.clone());
    put("CreatorUid", resource.creator_uid.clone());
    put("MIMEType", Some(resource.mime_type.clone()));
    put("Size", Some(resource.size.to_string()));
    put(
        "EXPIRATION",
        Some(
            resource
                .expiration
                .map_or_else(|| NEVER.to_string(), |at| at.to_string()),
        ),
    );
    put("Tool", Some(resource.tool.clone()));
    put("MissionName", resource.mission_name.clone());
    put("Altitude", resource.altitude.map(|v| v.to_string()));
    put("Latitude", resource.latitude.map(|v| v.to_string()));
    put("Longitude", resource.longitude.map(|v| v.to_string()));
    put("DownloadPath", resource.download_path.clone());
    put("Remarks", resource.remarks.clone());
    put("PluginClassName", resource.plugin_class_name.clone());

    let mut put_list = |key: &str, values: Vec<String>| {
        if !values.is_empty() {
            object.insert(
                key.to_string(),
                Value::Array(values.into_iter().map(Value::String).collect()),
            );
        }
    };

    put_list("Keywords", resource.keywords.clone());
    put_list("Groups", resource.groups.clone());
    put_list("Permissions", split_list(resource.permissions.as_deref()));
    put_list("Contacts", split_list(resource.contacts.as_deref()));

    Value::Object(object)
}

/// Whether a row can be rendered without breaking a client's parser.
///
/// ATAK treats a search result containing one bad row as a failed search, so
/// this is asked before a row joins a listing rather than after.
pub fn is_renderable(resource: &ResourceRow) -> bool {
    resource.id >= 0
        && !resource.uid.is_empty()
        && !resource.name.is_empty()
        && !resource.hash.is_empty()
}

/// The `{resultCount, results}` body of `GET /Marti/sync/search`.
///
/// `resultCount` is a real JSON number even though everything inside a result
/// is a string — ATAK's browser checks for the literal key before it parses.
pub fn search_results(resources: &[ResourceRow]) -> Value {
    let results: Vec<Value> = resources
        .iter()
        .filter(|resource| is_renderable(resource))
        .map(metadata)
        .collect();

    serde_json::json!({ "resultCount": results.len(), "results": results })
}

/// The flat display map `GET /Marti/api/files/metadata` emits per resource.
///
/// A [`BTreeMap`] so that the keys come out in a stable order, which is what
/// makes a golden test of this endpoint worth having.
pub fn files_entry(resource: &ResourceRow) -> BTreeMap<String, String> {
    let mut entry = metadata_entry(resource);
    entry.insert("Groups".to_string(), resource.groups.join(","));
    entry.insert(
        "Time".to_string(),
        time::java_date_string(resource.submission_time),
    );

    entry
}

/// The same map `HEAD /Marti/api/files/{hash}` emits: no `Groups`, and `Time`
/// is the padded instant rather than the Java rendering.
pub fn metadata_entry(resource: &ResourceRow) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("Name".to_string(), resource.name.clone()),
        (
            "User".to_string(),
            resource.submitter.clone().unwrap_or_default(),
        ),
        (
            "Creator".to_string(),
            resource.creator_uid.clone().unwrap_or_default(),
        ),
        ("Size".to_string(), humanise(resource.size)),
        ("Time".to_string(), time::cot_date(resource.submission_time)),
        ("MimeType".to_string(), resource.mime_type.clone()),
        ("Keywords".to_string(), resource.keywords.join(",")),
        ("Expiration".to_string(), expiration_text(resource)),
        ("Hash".to_string(), resource.hash.clone()),
    ])
}

/// `Expiration` as the file manager shows it: an instant with the trailing `Z`
/// chopped off, or the literal `none`.
fn expiration_text(resource: &ResourceRow) -> String {
    match resource.expiration.filter(|at| *at >= 0) {
        None => "none".to_string(),
        Some(at) => chrono::DateTime::from_timestamp_millis(at)
            .map(|at| time::cot_date(at).trim_end_matches('Z').to_string())
            .unwrap_or_else(|| "none".to_string()),
    }
}

/// A byte count as a person reads it: `912B`, `12kB`, `3MB`, `1GB`.
///
/// The thresholds and the spelling are TAK's, including the lower-case `k`.
pub fn humanise(size: i64) -> String {
    const UNITS: &[(i64, &str)] = &[
        (1024 * 1024 * 1024, "GB"),
        (1024 * 1024, "MB"),
        (1024, "kB"),
    ];

    if size < 0 {
        return UNKNOWN_SIZE.to_string();
    }

    for (scale, suffix) in UNITS {
        if size >= *scale {
            return format!("{}{suffix}", size / scale);
        }
    }

    format!("{size}B")
}

/// Splits one of the comma-joined columns back into its values.
fn split_list(stored: Option<&str>) -> Vec<String> {
    stored
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    fn row() -> ResourceRow {
        ResourceRow {
            id: 7,
            hash: "aa".to_string(),
            uid: "uid-1".to_string(),
            name: "package.zip".to_string(),
            filename: Some("package.zip".to_string()),
            mime_type: "application/x-zip-compressed".to_string(),
            size: 12_345,
            tool: "public".to_string(),
            creator_uid: Some("ANDROID-1".to_string()),
            submitter_id: None,
            submitter: Some("grace".to_string()),
            submission_time: chrono::Utc.with_ymd_and_hms(2024, 5, 1, 12, 0, 0).unwrap(),
            expiration: None,
            is_mission_package: true,
            groups: vec!["Blue".to_string(), "Red".to_string()],
            mission_name: None,
            latitude: None,
            longitude: None,
            altitude: None,
            remarks: None,
            permissions: None,
            contacts: None,
            download_path: None,
            plugin_class_name: None,
            install_on_enrollment: false,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            keywords: vec!["missionpackage".to_string()],
        }
    }

    #[test]
    fn the_numbers_a_client_parses_are_emitted_as_strings() {
        let json = metadata(&row());

        assert_eq!(json["Size"], "12345");
        assert_eq!(json["PrimaryKey"], "7");
        assert!(json["Size"].is_string() && json["PrimaryKey"].is_string());
        assert!(
            json["PrimaryKey"].as_str().unwrap().parse::<u64>().is_ok(),
            "ATAK refuses the whole response over an unparseable PrimaryKey",
        );
    }

    #[test]
    fn expiration_is_always_present_and_minus_one_when_unset() {
        // CloudTAK reads the key unconditionally; an absent one is `undefined`
        // where it expects a value.
        assert_eq!(metadata(&row())["EXPIRATION"], NEVER);
        assert_eq!(
            metadata(&ResourceRow {
                expiration: Some(1_714_564_800_000),
                ..row()
            })["EXPIRATION"],
            "1714564800000",
        );
    }

    #[test]
    fn the_submission_instant_is_padded_to_three_digits() {
        assert_eq!(
            metadata(&row())["SubmissionDateTime"],
            "2024-05-01T12:00:00.000Z",
        );
    }

    #[test]
    fn an_absent_value_is_left_out_rather_than_sent_as_null() {
        let json = metadata(&row());

        assert!(json.get("Remarks").is_none());
        assert!(json.get("Latitude").is_none());
        assert!(json.get("MissionName").is_none());
        assert!(json["Keywords"].is_array());
        assert_eq!(json["Groups"][0], "Blue");
    }

    #[test]
    fn a_row_that_would_break_a_browser_never_joins_a_listing() {
        let broken = ResourceRow {
            uid: String::new(),
            ..row()
        };

        assert!(!is_renderable(&broken));

        let body = search_results(&[row(), broken]);

        assert_eq!(body["resultCount"], 1);
        assert!(body["resultCount"].is_number(), "ATAK looks for the number");
        assert_eq!(body["results"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_file_manager_map_is_the_one_cloudtak_reads_channels_out_of() {
        let entry = files_entry(&row());

        assert_eq!(entry["Hash"], "aa");
        assert_eq!(entry["Groups"], "Blue,Red");
        assert_eq!(entry["Time"], "Wed May 01 12:00:00 UTC 2024");
        assert_eq!(entry["Size"], "12kB");
        assert_eq!(entry["Expiration"], "none");
        assert_eq!(entry["User"], "grace");
        assert_eq!(entry["Creator"], "ANDROID-1");
    }

    #[test]
    fn the_head_map_drops_groups_and_reports_the_instant() {
        let entry = metadata_entry(&row());

        assert!(!entry.contains_key("Groups"));
        assert_eq!(entry["Time"], "2024-05-01T12:00:00.000Z");
    }

    #[test]
    fn sizes_are_humanised_the_way_the_file_manager_shows_them() {
        assert_eq!(humanise(0), "0B");
        assert_eq!(humanise(912), "912B");
        assert_eq!(humanise(12 * 1024), "12kB");
        assert_eq!(humanise(3 * 1024 * 1024), "3MB");
        assert_eq!(humanise(2 * 1024 * 1024 * 1024), "2GB");
        assert_eq!(humanise(-1), "Unknown");
    }
}
