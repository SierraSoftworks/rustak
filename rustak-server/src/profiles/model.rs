//! `profiles`, `profile_files` and `profile_prefs`: the rows, and what they
//! mean.
//!
//! A profile is a named bundle with two delivery flags and a channel list. The
//! flags decide which endpoint offers it; the channel list decides who sees it,
//! with an empty list meaning everybody. A file's bytes live in the content
//! store and only its path, hash and size are here.
//!
//! The SQL that reads and writes these rows is in [`super::repo`].

use chrono::{DateTime, Utc};
use rustak_api::{GroupName, ProfileId};

use crate::db::row::{bool_col, id_col, ts};

/// The columns [`ProfileRow::from_row`] expects, in order.
pub(super) const COLUMNS: &str = "id, name, description, enabled, apply_on_enrollment, apply_on_connect, \
     tool, type, groups, created_at, updated_at";

/// The columns [`ProfileFileRow::from_row`] expects, in order.
pub(super) const FILE_COLUMNS: &str = "id, profile_id, path, hash, size, mime_type, updated_at";

/// One row of `profiles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRow {
    pub id: ProfileId,
    pub name: String,
    pub description: Option<String>,
    /// Whether it is delivered at all.
    pub active: bool,
    pub apply_on_enrollment: bool,
    pub apply_on_connect: bool,
    pub tool: Option<String>,
    pub kind: Option<String>,
    /// Empty means everybody.
    pub groups: Vec<GroupName>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProfileRow {
    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            active: bool_col(row, 3)?,
            apply_on_enrollment: bool_col(row, 4)?,
            apply_on_connect: bool_col(row, 5)?,
            tool: row.get(6)?,
            kind: row.get(7)?,
            groups: crate::db::row::json_col::<Vec<String>>(row, 8)?
                .into_iter()
                .filter_map(|name| GroupName::parse(&name).ok())
                .collect(),
            created_at: ts(row, 9)?,
            updated_at: ts(row, 10)?,
        })
    }

    /// Whether somebody holding `held` should receive this profile.
    ///
    /// An empty channel list is "everybody", which is what makes a first
    /// profile useful before an operator has set any channels up.
    pub fn visible_to(&self, held: &[GroupName]) -> bool {
        self.groups.is_empty() || self.groups.iter().any(|group| held.contains(group))
    }
}

/// One row of `profile_files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFileRow {
    pub id: i64,
    pub profile_id: ProfileId,
    /// The delivered path, which is what a `relativePath` query matches.
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub mime_type: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl ProfileFileRow {
    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            profile_id: id_col(row, 1)?,
            path: row.get(2)?,
            hash: row.get(3)?,
            size: row.get::<_, i64>(4)?.max(0) as u64,
            mime_type: row.get(5)?,
            updated_at: ts(row, 6)?,
        })
    }
}

/// A profile about to be created.
#[derive(Debug, Clone, Default)]
pub struct NewProfile {
    pub name: String,
    pub description: Option<String>,
    pub active: bool,
    pub apply_on_enrollment: bool,
    pub apply_on_connect: bool,
    pub tool: Option<String>,
    pub kind: Option<String>,
    pub groups: Vec<GroupName>,
}

/// Which endpoint is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `GET /Marti/api/tls/profile/enrollment`.
    Enrollment,
    /// `GET /Marti/api/device/profile/connection`.
    Connect,
    /// `GET /Marti/api/device/profile/tool/{tool}`.
    Tool(String),
}

impl Delivery {
    /// The `WHERE` fragment that selects the profiles this delivery offers.
    pub(super) fn predicate(&self) -> &'static str {
        match self {
            Self::Enrollment => "apply_on_enrollment = 1",
            Self::Connect => "apply_on_connect = 1",
            Self::Tool(_) => "tool = ?1",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_channel_list_means_everybody() {
        let row = ProfileRow {
            id: ProfileId::new(1),
            name: "P".to_string(),
            description: None,
            active: true,
            apply_on_enrollment: true,
            apply_on_connect: false,
            tool: None,
            kind: None,
            groups: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(row.visible_to(&[]));

        let scoped = ProfileRow {
            groups: vec![GroupName::parse("Blue").unwrap()],
            ..row
        };

        assert!(!scoped.visible_to(&[]));
        assert!(scoped.visible_to(&[GroupName::parse("Blue").unwrap()]));
        assert!(!scoped.visible_to(&[GroupName::parse("Red").unwrap()]));
    }

    #[test]
    fn a_delivery_selects_on_the_column_its_endpoint_means() {
        assert_eq!(Delivery::Enrollment.predicate(), "apply_on_enrollment = 1");
        assert_eq!(Delivery::Connect.predicate(), "apply_on_connect = 1");
        assert_eq!(Delivery::Tool("x".into()).predicate(), "tool = ?1");
    }
}
