//! The shape of a `resources` row, and the two descriptions of one.
//!
//! Split from the repository itself so that the SQL and the data are read
//! separately: this file says what a resource *is* — every column, what a new
//! one carries, which fields a client may change and how a listing narrows —
//! and [`super`] says how those reach SQLite.

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rustak_core::prelude::*;

use crate::db::row::{Timestamp, bool_col, json_col, opt_id_col, opt_ts, ts};

/// The columns [`ResourceRow::from_row`] expects, in order.
pub(super) const COLUMNS: &str = "id, hash, uid, name, filename, mime_type, size, tool, creator_uid, \
     submitter_id, submitter, submission_time, expiration, is_mission_package, groups, \
     mission_name, latitude, longitude, altitude, remarks, permissions, contacts, \
     download_path, plugin_class_name, install_on_enrollment, deleted_at, created_at";

/// One row of `resources`, with its keywords already joined on.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceRow {
    /// The legacy `PrimaryKey`.
    pub id: i64,
    /// The SHA-256 of the stored bytes.
    pub hash: String,
    /// How a client addresses this resource.
    pub uid: String,
    pub name: String,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size: i64,
    pub tool: String,
    pub creator_uid: Option<String>,
    pub submitter_id: Option<UserId>,
    pub submitter: Option<String>,
    pub submission_time: DateTime<Utc>,
    /// Epoch milliseconds; `None` means never.
    pub expiration: Option<i64>,
    pub is_mission_package: bool,
    /// The channels that may see it. Empty means `__ANON__`.
    pub groups: Vec<String>,
    pub mission_name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub remarks: Option<String>,
    pub permissions: Option<String>,
    pub contacts: Option<String>,
    pub download_path: Option<String>,
    pub plugin_class_name: Option<String>,
    /// Ship this package in the enrolment profile.
    pub install_on_enrollment: bool,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// Filled by [`super::ResourcesRepo`] from `resource_keywords`.
    pub keywords: Vec<String>,
}

impl ResourceRow {
    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            hash: row.get(1)?,
            uid: row.get(2)?,
            name: row.get(3)?,
            filename: row.get(4)?,
            mime_type: row.get(5)?,
            size: row.get(6)?,
            tool: row.get(7)?,
            creator_uid: row.get(8)?,
            submitter_id: opt_id_col(row, 9)?,
            submitter: row.get(10)?,
            submission_time: ts(row, 11)?,
            expiration: row.get(12)?,
            is_mission_package: bool_col(row, 13)?,
            groups: json_col(row, 14)?,
            mission_name: row.get(15)?,
            latitude: row.get(16)?,
            longitude: row.get(17)?,
            altitude: row.get(18)?,
            remarks: row.get(19)?,
            permissions: row.get(20)?,
            contacts: row.get(21)?,
            download_path: row.get(22)?,
            plugin_class_name: row.get(23)?,
            install_on_enrollment: bool_col(row, 24)?,
            deleted_at: opt_ts(row, 25)?,
            created_at: ts(row, 26)?,
            keywords: Vec::new(),
        })
    }
}

/// A resource about to be stored.
#[derive(Debug, Clone, Default)]
pub struct NewResource {
    pub hash: String,
    pub uid: String,
    pub name: String,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size: i64,
    pub tool: String,
    pub creator_uid: Option<String>,
    pub submitter_id: Option<UserId>,
    pub submitter: Option<String>,
    pub expiration: Option<i64>,
    pub is_mission_package: bool,
    pub groups: Vec<String>,
    pub mission_name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub remarks: Option<String>,
    pub permissions: Option<String>,
    pub contacts: Option<String>,
    pub download_path: Option<String>,
    pub plugin_class_name: Option<String>,
    pub keywords: Vec<String>,
}

/// Which mutable metadata field `/Marti/api/sync/metadata` is changing.
///
/// Only these two: TAK's own API refuses any other path segment, and a client
/// that could rename or re-own somebody else's upload through it would be a
/// way around the visibility rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutableField {
    /// `tool` — which client surface the resource belongs to.
    Tool,
    /// `mimetype` — the content type served on download.
    MimeType,
}

impl MutableField {
    /// Reads the path segment, case-insensitively as TAK does.
    pub fn parse(segment: &str) -> Option<Self> {
        match segment.trim().to_ascii_lowercase().as_str() {
            "tool" => Some(Self::Tool),
            "mimetype" => Some(Self::MimeType),
            _ => None,
        }
    }

    /// The column it writes.
    pub(super) fn column(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::MimeType => "mime_type",
        }
    }
}

/// Which rows a listing wants.
///
/// Every field is an `AND`; an unset field is not asked about. The string
/// fields are exact matches rather than substrings, which is what both the
/// legacy servlet and the modern API do — a package browser searches by
/// keyword, not by prefix.
#[derive(Debug, Clone, Default)]
pub struct ResourceFilter {
    pub id: Option<i64>,
    pub uid: Option<String>,
    pub hash: Option<String>,
    pub name: Option<String>,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub tool: Option<String>,
    pub mission_name: Option<String>,
    /// Every keyword must be present.
    pub keywords: Vec<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    /// Oldest first. The default is newest first, which is what a browser shows.
    pub ascending: bool,
}

impl ResourceFilter {
    /// Everything with this hash.
    pub fn by_hash(hash: impl Into<String>) -> Self {
        Self {
            hash: Some(hash.into()),
            ..Self::default()
        }
    }

    /// The `WHERE` body and the values it binds.
    pub(super) fn clauses(&self) -> (String, Vec<Value>) {
        let mut sql = vec!["deleted_at IS NULL".to_string()];
        let mut binds: Vec<Value> = Vec::new();

        let equalities = [
            ("uid", self.uid.as_ref()),
            ("hash", self.hash.as_ref()),
            ("name", self.name.as_ref()),
            ("filename", self.filename.as_ref()),
            ("mime_type", self.mime_type.as_ref()),
            ("tool", self.tool.as_ref()),
            ("mission_name", self.mission_name.as_ref()),
        ];

        for (column, value) in equalities {
            if let Some(value) = value {
                binds.push(Value::Text(value.clone()));
                sql.push(format!("{column} = ?{}", binds.len()));
            }
        }

        if let Some(id) = self.id {
            binds.push(Value::Integer(id));
            sql.push(format!("id = ?{}", binds.len()));
        }

        for (comparison, bound) in [(">=", self.start), ("<=", self.end)] {
            if let Some(at) = bound {
                binds.push(Value::Text(Timestamp::from(at).to_text()));
                sql.push(format!("submission_time {comparison} ?{}", binds.len()));
            }
        }

        for keyword in &self.keywords {
            binds.push(Value::Text(keyword.clone()));
            sql.push(format!(
                "EXISTS (SELECT 1 FROM resource_keywords k \
                 WHERE k.resource_id = resources.id AND k.keyword = ?{})",
                binds.len()
            ));
        }

        (sql.join(" AND "), binds)
    }

    /// The `ORDER BY … LIMIT …` tail.
    pub(super) fn tail(&self) -> String {
        let direction = if self.ascending { "ASC" } else { "DESC" };
        let mut tail = format!("ORDER BY submission_time {direction}, id {direction}");

        if let Some(limit) = self.limit {
            tail.push_str(&format!(" LIMIT {limit}"));
            tail.push_str(&format!(" OFFSET {}", self.offset.unwrap_or(0)));
        }

        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tool_and_mimetype_are_addressable_metadata() {
        assert_eq!(MutableField::parse("TOOL"), Some(MutableField::Tool));
        assert_eq!(
            MutableField::parse("mimetype"),
            Some(MutableField::MimeType)
        );
        assert_eq!(MutableField::parse("name"), None);
        assert_eq!(MutableField::parse("groups"), None);
    }

    #[test]
    fn an_unasked_question_narrows_nothing_but_the_deleted_rows() {
        let (sql, binds) = ResourceFilter::default().clauses();

        assert_eq!(sql, "deleted_at IS NULL");
        assert!(binds.is_empty());
    }

    #[test]
    fn every_clause_binds_in_the_order_it_was_numbered() {
        let (sql, binds) = ResourceFilter {
            hash: Some("aa".to_string()),
            tool: Some("public".to_string()),
            keywords: vec!["missionpackage".to_string()],
            ..ResourceFilter::default()
        }
        .clauses();

        assert_eq!(binds.len(), 3);
        assert!(sql.contains("hash = ?1"), "{sql}");
        assert!(sql.contains("tool = ?2"), "{sql}");
        assert!(sql.contains("k.keyword = ?3"), "{sql}");
    }

    #[test]
    fn a_listing_is_newest_first_and_only_paged_when_asked() {
        assert_eq!(
            ResourceFilter::default().tail(),
            "ORDER BY submission_time DESC, id DESC",
        );
        assert_eq!(
            ResourceFilter {
                limit: Some(10),
                offset: Some(20),
                ascending: true,
                ..ResourceFilter::default()
            }
            .tail(),
            "ORDER BY submission_time ASC, id ASC LIMIT 10 OFFSET 20",
        );
    }
}
