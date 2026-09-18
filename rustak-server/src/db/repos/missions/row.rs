//! The `missions` row, and the two shapes that write it.
//!
//! The row is deliberately storage-shaped rather than wire-shaped: `groups`,
//! `keywords` and `bounding_polygon` come back as `Vec<String>` because that is
//! what the JSON columns hold, and `default_role` comes back as the stored
//! string rather than as a parsed enum so that this file does not have to know
//! what a mission role means. [`crate::missions::model`] does that conversion.

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use uuid::Uuid;

use crate::db::row::{bool_col, json_col, opt_json_col, opt_ts, ts};

/// Every column [`MissionRow::from_row`] reads, in order.
pub const COLUMNS: &str = "id, guid, name, description, chat_room, base_layer, bbox, \
     bounding_polygon, path, classification, tool, keywords, creator_uid, create_time, \
     last_edited, default_role, invite_only, password_hash, expiration, groups, parent_id, \
     deleted_at";

/// One stored mission.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionRow {
    /// The primary key, which nothing on the wire ever sees.
    pub id: i64,
    /// The immutable identifier every client prefers.
    pub guid: Uuid,
    /// The human label, and what `<dest mission=…>` names.
    pub name: String,
    /// Always a string on the wire, so empty rather than absent here.
    pub description: String,
    pub chat_room: Option<String>,
    pub base_layer: Option<String>,
    pub bbox: Option<String>,
    /// `"lat,lon"` strings, in the order the client sent them.
    pub bounding_polygon: Vec<String>,
    pub path: Option<String>,
    pub classification: Option<String>,
    /// Which client family owns the mission; `"public"` unless one said.
    pub tool: String,
    pub keywords: Vec<String>,
    pub creator_uid: Option<String>,
    pub create_time: DateTime<Utc>,
    pub last_edited: Option<DateTime<Utc>>,
    /// The stored spelling of the role an unlisted caller gets.
    pub default_role: String,
    pub invite_only: bool,
    /// argon2id; see [`crate::missions`] for why this is not TAK's bcrypt.
    pub password_hash: Option<String>,
    /// Epoch seconds, or `None` for a mission that never expires.
    pub expiration: Option<i64>,
    /// The channels the mission is shared with; empty means `__ANON__`.
    pub groups: Vec<String>,
    pub parent_id: Option<i64>,
    /// Set by a soft delete, which is what makes a later `GET` a `410`.
    pub deleted_at: Option<DateTime<Utc>>,
}

impl MissionRow {
    /// Reads a row selected with [`COLUMNS`].
    ///
    /// # Errors
    ///
    /// Whatever SQLite or the JSON columns reported.
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            guid: parse_guid(row, 1)?,
            name: row.get(2)?,
            description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            chat_room: row.get(4)?,
            base_layer: row.get(5)?,
            bbox: row.get(6)?,
            bounding_polygon: opt_json_col(row, 7)?.unwrap_or_default(),
            path: row.get(8)?,
            classification: row.get(9)?,
            tool: row.get(10)?,
            keywords: json_col(row, 11)?,
            creator_uid: row.get(12)?,
            create_time: ts(row, 13)?,
            last_edited: opt_ts(row, 14)?,
            default_role: row.get(15)?,
            invite_only: bool_col(row, 16)?,
            password_hash: row.get(17)?,
            expiration: row.get(18)?,
            groups: json_col(row, 19)?,
            parent_id: row.get(20)?,
            deleted_at: opt_ts(row, 21)?,
        })
    }

    /// Whether this mission has been soft-deleted.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}

/// Reads a `TEXT` column holding a uuid.
fn parse_guid(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let text: String = row.get(index)?;

    Uuid::parse_str(&text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

/// What a create supplies.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMission {
    pub guid: Uuid,
    pub name: String,
    pub description: String,
    pub chat_room: Option<String>,
    pub base_layer: Option<String>,
    pub bbox: Option<String>,
    pub bounding_polygon: Vec<String>,
    pub path: Option<String>,
    pub classification: Option<String>,
    pub tool: String,
    pub keywords: Vec<String>,
    pub creator_uid: Option<String>,
    pub default_role: String,
    pub invite_only: bool,
    pub password_hash: Option<String>,
    pub expiration: Option<i64>,
    pub groups: Vec<String>,
    pub parent_id: Option<i64>,
}

impl NewMission {
    /// A mission with everything at its default, for a caller to fill in.
    pub fn new(name: impl Into<String>, default_role: impl Into<String>) -> Self {
        Self {
            guid: Uuid::new_v4(),
            name: name.into(),
            description: String::new(),
            chat_room: None,
            base_layer: None,
            bbox: None,
            bounding_polygon: Vec::new(),
            path: None,
            classification: None,
            tool: "public".to_string(),
            keywords: Vec::new(),
            creator_uid: None,
            default_role: default_role.into(),
            invite_only: false,
            password_hash: None,
            expiration: None,
            groups: Vec::new(),
            parent_id: None,
        }
    }
}

/// What an update changes.
///
/// Every field is a double option: the outer one says whether the caller
/// mentioned the field at all, the inner one whether they asked for it to be
/// cleared. A `PUT` that names only `description` must not blank the chat room.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MissionPatch {
    pub description: Option<String>,
    pub chat_room: Option<Option<String>>,
    pub base_layer: Option<Option<String>>,
    pub bbox: Option<Option<String>>,
    pub bounding_polygon: Option<Vec<String>>,
    pub path: Option<Option<String>>,
    pub classification: Option<Option<String>>,
    pub tool: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub default_role: Option<String>,
    pub invite_only: Option<bool>,
    /// `Some(None)` clears the password; `None` leaves it alone.
    pub password_hash: Option<Option<String>>,
    pub expiration: Option<Option<i64>>,
    pub groups: Option<Vec<String>>,
    pub parent_id: Option<Option<i64>>,
}

impl MissionPatch {
    /// Whether this patch would change anything at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Which missions a listing wants.
///
/// Only the narrowing SQL can do cheaply lives here; the group, password and
/// default-role rules are applied in Rust over the rows, for the reason
/// [`crate::files::metadata`] gives about bit vectors and JSON arrays.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MissionFilter {
    /// The owning tool. `None` means every tool, which only the admin API asks
    /// for — the Marti listing always passes one.
    pub tool: Option<String>,
    /// An exact name, matched case-insensitively by the column's collation.
    pub name: Option<String>,
    /// An exact guid.
    pub guid: Option<Uuid>,
    /// The parent whose children are wanted.
    pub parent_id: Option<i64>,
    /// Whether soft-deleted missions are included, which only a resolve by
    /// name or guid asks for — so that it can answer `410` rather than `404`.
    pub include_deleted: bool,
}

impl MissionFilter {
    /// Every live mission owned by one tool.
    pub fn tool(tool: impl Into<String>) -> Self {
        Self {
            tool: Some(tool.into()),
            ..Self::default()
        }
    }

    /// One mission by name, deleted rows included.
    pub fn by_name(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            include_deleted: true,
            ..Self::default()
        }
    }

    /// One mission by guid, deleted rows included.
    pub fn by_guid(guid: Uuid) -> Self {
        Self {
            guid: Some(guid),
            include_deleted: true,
            ..Self::default()
        }
    }

    /// The `WHERE` clause and the values it binds.
    pub(super) fn clauses(&self) -> (String, Vec<Value>) {
        let mut clauses = vec!["1 = 1".to_string()];
        let mut binds: Vec<Value> = Vec::new();

        if !self.include_deleted {
            clauses.push("deleted_at IS NULL".to_string());
        }

        let equalities = [
            ("tool", self.tool.clone()),
            ("name", self.name.clone()),
            ("guid", self.guid.map(|guid| guid.to_string())),
        ];

        for (column, value) in equalities {
            if let Some(value) = value {
                binds.push(Value::Text(value));
                clauses.push(format!("{column} = ?{}", binds.len()));
            }
        }

        if let Some(parent) = self.parent_id {
            binds.push(Value::Integer(parent));
            clauses.push(format!("parent_id = ?{}", binds.len()));
        }

        (clauses.join(" AND "), binds)
    }
}
