//! `mission_changes`: what happened to a mission, in the order it happened.
//!
//! Append-only. Nothing updates a change row and nothing deletes one short of
//! the mission going away, because the log *is* the answer to "what has
//! happened since I last looked" — the question every subscribed client asks on
//! every reconnect.
//!
//! # Why the squash is not in here
//!
//! `GET …/changes?squashed=true` wants the current-state delta rather than the
//! history, and computing that in SQL means a six-way union over this table
//! joined against two others. It is a fold over the window instead
//! ([`crate::missions::changes`]), which is testable against a naive model and
//! costs nothing at these volumes.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{Database, row::Timestamp, row::bool_col, row::opt_json_col, row::ts};

/// Every column [`MissionChangeRow::from_row`] reads, in order.
const COLUMNS: &str = "id, mission_id, type, timestamp, server_time, creator_uid, content_uid, \
     content_hash, log_entry_id, map_layer_uid, feed_uid, is_federated, detail";

/// One recorded change.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionChangeRow {
    pub id: i64,
    pub mission_id: i64,
    /// The stored spelling: `ADD_CONTENT`, `REMOVE_CONTENT`, `CREATE_MISSION`,
    /// `DELETE_MISSION`, `CREATE_DATA_FEED` or `DELETE_DATA_FEED`.
    pub kind: String,
    /// When the change happened, as the client dated it.
    pub timestamp: DateTime<Utc>,
    /// When we recorded it, which is what a time window is applied to.
    pub server_time: DateTime<Utc>,
    pub creator_uid: Option<String>,
    /// The map item this change is about.
    pub content_uid: Option<String>,
    /// The resource this change is about.
    pub content_hash: Option<String>,
    pub log_entry_id: Option<String>,
    pub map_layer_uid: Option<String>,
    pub feed_uid: Option<String>,
    /// Always false here; federation is not implemented and the field is part
    /// of the wire shape.
    pub is_federated: bool,
    /// The cached item details a client renders without fetching the CoT.
    pub detail: Option<serde_json::Value>,
}

impl MissionChangeRow {
    /// Reads a row selected with [`COLUMNS`].
    ///
    /// # Errors
    ///
    /// Whatever SQLite or the JSON column reported.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            mission_id: row.get(1)?,
            kind: row.get(2)?,
            timestamp: ts(row, 3)?,
            server_time: ts(row, 4)?,
            creator_uid: row.get(5)?,
            content_uid: row.get(6)?,
            content_hash: row.get(7)?,
            log_entry_id: row.get(8)?,
            map_layer_uid: row.get(9)?,
            feed_uid: row.get(10)?,
            is_federated: bool_col(row, 11)?,
            detail: opt_json_col(row, 12)?,
        })
    }
}

/// What a caller appends.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChange {
    pub mission_id: i64,
    pub kind: String,
    pub timestamp: DateTime<Utc>,
    pub creator_uid: Option<String>,
    pub content_uid: Option<String>,
    pub content_hash: Option<String>,
    pub log_entry_id: Option<String>,
    pub map_layer_uid: Option<String>,
    pub feed_uid: Option<String>,
    pub detail: Option<serde_json::Value>,
}

impl NewChange {
    /// A change of one kind against one mission, for a caller to fill in.
    pub fn new(mission_id: i64, kind: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            mission_id,
            kind: kind.into(),
            timestamp: at,
            creator_uid: None,
            content_uid: None,
            content_hash: None,
            log_entry_id: None,
            map_layer_uid: None,
            feed_uid: None,
            detail: None,
        }
    }

    /// Names who made the change.
    #[must_use]
    pub fn by(mut self, creator_uid: Option<&str>) -> Self {
        self.creator_uid = creator_uid.map(str::to_string);
        self
    }

    /// Names the map item the change is about.
    #[must_use]
    pub fn about_uid(mut self, uid: impl Into<String>) -> Self {
        self.content_uid = Some(uid.into());
        self
    }

    /// Names the resource the change is about.
    #[must_use]
    pub fn about_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }

    /// Attaches the cached item details.
    #[must_use]
    pub fn with_detail(mut self, detail: Option<serde_json::Value>) -> Self {
        self.detail = detail;
        self
    }
}

/// Reads and appends `mission_changes`.
pub struct MissionChangesRepo<'a> {
    db: &'a Database,
}

impl<'a> MissionChangesRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Appends one change, stamping `server_time`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn record(&self, change: NewChange) -> Result<MissionChangeRow, Error> {
        self.record_all(vec![change])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                human_errors::system(
                    "A mission change was written and could not be read back.",
                    crate::db::ADVICE_REPORT_DEV,
                )
            })
    }

    /// Appends several changes in one transaction.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn record_all(
        &self,
        changes: Vec<NewChange>,
    ) -> Result<Vec<MissionChangeRow>, Error> {
        if changes.is_empty() {
            return Ok(Vec::new());
        }

        self.db
            .write(move |tx| {
                let now = Timestamp::now();
                let mut written = Vec::with_capacity(changes.len());

                for change in changes {
                    written.push(tx.query_one(
                        &format!(
                            "INSERT INTO mission_changes (mission_id, type, timestamp, \
                               server_time, creator_uid, content_uid, content_hash, \
                               log_entry_id, map_layer_uid, feed_uid, detail) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                             RETURNING {COLUMNS}"
                        ),
                        rusqlite::params![
                            change.mission_id,
                            change.kind,
                            Timestamp::from(change.timestamp),
                            now,
                            change.creator_uid,
                            change.content_uid,
                            change.content_hash,
                            change.log_entry_id,
                            change.map_layer_uid,
                            change.feed_uid,
                            change.detail.as_ref().map(|detail| detail.to_string()),
                        ],
                        MissionChangeRow::from_row,
                    )?);
                }

                Ok(written)
            })
            .await
    }

    /// Every change recorded inside a window, newest first.
    ///
    /// Bounded on `server_time` rather than `timestamp`: a client dates its own
    /// change and two clients' clocks do not agree, so "since I last asked" can
    /// only be answered by when *we* saw it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn window(
        &self,
        mission_id: i64,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<MissionChangeRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_changes \
                     WHERE mission_id = ?1 AND server_time >= ?2 AND server_time <= ?3 \
                     ORDER BY server_time DESC, id DESC"
                ))?;

                statement
                    .query_map(
                        rusqlite::params![mission_id, Timestamp::from(start), Timestamp::from(end)],
                        MissionChangeRow::from_row,
                    )?
                    .collect()
            })
            .await
    }

    /// The most recent change of a kind about one item, if any.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn latest_for_uid(
        &self,
        mission_id: i64,
        uid: String,
    ) -> Result<Option<MissionChangeRow>, Error> {
        self.db
            .read(move |c| {
                c.query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM mission_changes \
                         WHERE mission_id = ?1 AND content_uid = ?2 \
                         ORDER BY server_time DESC, id DESC LIMIT 1"
                    ),
                    rusqlite::params![mission_id, uid],
                    MissionChangeRow::from_row,
                )
                .optional()
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::repos::missions::NewMission;

    use super::*;

    async fn mission(db: &Database) -> i64 {
        db.missions()
            .create(NewMission::new("Alpha", "MISSION_SUBSCRIBER"))
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn changes_come_back_newest_first_inside_the_window() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db).await;
        let now = Utc::now();

        db.mission_changes()
            .record_all(vec![
                NewChange::new(id, "ADD_CONTENT", now).about_uid("a"),
                NewChange::new(id, "ADD_CONTENT", now).about_uid("b"),
                NewChange::new(id, "REMOVE_CONTENT", now).about_uid("a"),
            ])
            .await
            .unwrap();

        let window = db
            .mission_changes()
            .window(
                id,
                now - chrono::Duration::hours(1),
                now + chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert_eq!(window.len(), 3);
        assert_eq!(window[0].kind, "REMOVE_CONTENT");
        assert_eq!(window[0].content_uid.as_deref(), Some("a"));
        assert!(!window[0].is_federated);
    }

    #[tokio::test]
    async fn a_window_that_ended_before_the_change_is_empty() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db).await;
        let now = Utc::now();

        db.mission_changes()
            .record(NewChange::new(id, "ADD_CONTENT", now).about_uid("a"))
            .await
            .unwrap();

        let window = db
            .mission_changes()
            .window(
                id,
                now - chrono::Duration::days(2),
                now - chrono::Duration::days(1),
            )
            .await
            .unwrap();

        assert!(window.is_empty());
    }

    #[tokio::test]
    async fn the_detail_column_round_trips_as_json() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db).await;
        let detail = serde_json::json!({ "type": "a-f-G", "callsign": "ALPHA" });

        let written = db
            .mission_changes()
            .record(
                NewChange::new(id, "ADD_CONTENT", Utc::now())
                    .about_uid("a")
                    .with_detail(Some(detail.clone())),
            )
            .await
            .unwrap();

        assert_eq!(written.detail, Some(detail));
    }
}
