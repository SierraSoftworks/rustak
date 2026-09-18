//! `mission_uids` and `mission_contents`: what is filed under a mission.
//!
//! Two tables, one repository, because they are always read together: a
//! mission's payload is its map items *and* its files, and a client asking for
//! one without the other has no use for the answer.
//!
//! # Why the details are cached on the uid row
//!
//! A mission's item list is rendered with each item's type, callsign, icon and
//! position, and the source of those is the CoT event itself. Reading every
//! event back to render a listing would make opening a Data Sync an O(items)
//! fan-out into the CoT store, so the fields a listing shows are copied onto
//! the row when the item is filed and are refreshed whenever it is filed again.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database, row::Timestamp, row::json_col, row::opt_json_col, row::to_json, row::ts,
};

/// Every column [`MissionUidRow::from_row`] reads, in order.
const UID_COLUMNS: &str =
    "mission_id, uid, creator_uid, timestamp, keywords, details, layer_uid, position";

/// Every column [`MissionContentRow::from_row`] reads, in order.
const CONTENT_COLUMNS: &str =
    "mission_id, resource_id, creator_uid, timestamp, keywords, layer_uid, position";

/// One map item filed under a mission.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionUidRow {
    pub mission_id: i64,
    pub uid: String,
    pub creator_uid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub keywords: Vec<String>,
    /// The cached rendering fields; see the module documentation.
    pub details: Option<serde_json::Value>,
    /// The layer this item is filed under, when it is filed under one.
    pub layer_uid: Option<String>,
    pub position: Option<i64>,
}

impl MissionUidRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            mission_id: row.get(0)?,
            uid: row.get(1)?,
            creator_uid: row.get(2)?,
            timestamp: ts(row, 3)?,
            keywords: json_col(row, 4)?,
            details: opt_json_col(row, 5)?,
            layer_uid: row.get(6)?,
            position: row.get(7)?,
        })
    }

    /// An item with no keywords, details or layer, for a caller to fill in.
    pub fn new(mission_id: i64, uid: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            mission_id,
            uid: uid.into(),
            creator_uid: None,
            timestamp: at,
            keywords: Vec::new(),
            details: None,
            layer_uid: None,
            position: None,
        }
    }
}

/// One resource filed under a mission.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionContentRow {
    pub mission_id: i64,
    pub resource_id: i64,
    pub creator_uid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub keywords: Vec<String>,
    pub layer_uid: Option<String>,
    pub position: Option<i64>,
}

impl MissionContentRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            mission_id: row.get(0)?,
            resource_id: row.get(1)?,
            creator_uid: row.get(2)?,
            timestamp: ts(row, 3)?,
            keywords: json_col(row, 4)?,
            layer_uid: row.get(5)?,
            position: row.get(6)?,
        })
    }

    /// A resource with no keywords or layer, for a caller to fill in.
    pub fn new(mission_id: i64, resource_id: i64, at: DateTime<Utc>) -> Self {
        Self {
            mission_id,
            resource_id,
            creator_uid: None,
            timestamp: at,
            keywords: Vec::new(),
            layer_uid: None,
            position: None,
        }
    }
}

/// Reads and writes `mission_uids` and `mission_contents`.
pub struct MissionContentsRepo<'a> {
    db: &'a Database,
}

impl<'a> MissionContentsRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Files a map item, refreshing it if it is already there.
    ///
    /// Reports whether the item was new, which is what decides between an
    /// `ADD_CONTENT` change and a silent refresh.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn upsert_uid(&self, item: MissionUidRow) -> Result<bool, Error> {
        self.db
            .write(move |tx| {
                let existed: bool = tx
                    .query_row(
                        "SELECT 1 FROM mission_uids WHERE mission_id = ?1 AND uid = ?2",
                        rusqlite::params![item.mission_id, item.uid],
                        |_| Ok(true),
                    )
                    .optional()?
                    .unwrap_or(false);

                tx.execute(
                    "INSERT INTO mission_uids \
                       (mission_id, uid, creator_uid, timestamp, keywords, details, layer_uid, position) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                     ON CONFLICT (mission_id, uid) DO UPDATE SET \
                       creator_uid = excluded.creator_uid, timestamp = excluded.timestamp, \
                       keywords = excluded.keywords, details = excluded.details, \
                       layer_uid = excluded.layer_uid, position = excluded.position",
                    rusqlite::params![
                        item.mission_id,
                        item.uid,
                        item.creator_uid,
                        Timestamp::from(item.timestamp),
                        to_json(&item.keywords)?,
                        item.details.as_ref().map(ToString::to_string),
                        item.layer_uid,
                        item.position,
                    ],
                )?;

                Ok(!existed)
            })
            .await
    }

    /// Unfiles a map item, reporting whether it was there.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn remove_uid(&self, mission_id: i64, uid: String) -> Result<bool, Error> {
        self.db
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM mission_uids WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid],
                )? > 0)
            })
            .await
    }

    /// Every map item filed under a mission, oldest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn uids(&self, mission_id: i64) -> Result<Vec<MissionUidRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {UID_COLUMNS} FROM mission_uids WHERE mission_id = ?1 \
                     ORDER BY position, timestamp, uid"
                ))?;

                statement
                    .query_map([mission_id], MissionUidRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Replaces a filed item's keywords, reporting whether it was there.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_uid_keywords(
        &self,
        mission_id: i64,
        uid: String,
        keywords: Vec<String>,
    ) -> Result<bool, Error> {
        self.db
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE mission_uids SET keywords = ?3 WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid, to_json(&keywords)?],
                )? > 0)
            })
            .await
    }

    /// Files a resource, refreshing it if it is already there.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn upsert_content(&self, content: MissionContentRow) -> Result<bool, Error> {
        self.db
            .write(move |tx| {
                let existed: bool = tx
                    .query_row(
                        "SELECT 1 FROM mission_contents WHERE mission_id = ?1 AND resource_id = ?2",
                        rusqlite::params![content.mission_id, content.resource_id],
                        |_| Ok(true),
                    )
                    .optional()?
                    .unwrap_or(false);

                tx.execute(
                    "INSERT INTO mission_contents \
                       (mission_id, resource_id, creator_uid, timestamp, keywords, layer_uid, position) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                     ON CONFLICT (mission_id, resource_id) DO UPDATE SET \
                       creator_uid = excluded.creator_uid, timestamp = excluded.timestamp, \
                       keywords = excluded.keywords, layer_uid = excluded.layer_uid, \
                       position = excluded.position",
                    rusqlite::params![
                        content.mission_id,
                        content.resource_id,
                        content.creator_uid,
                        Timestamp::from(content.timestamp),
                        to_json(&content.keywords)?,
                        content.layer_uid,
                        content.position,
                    ],
                )?;

                Ok(!existed)
            })
            .await
    }

    /// Every resource filed under a mission, oldest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn contents(&self, mission_id: i64) -> Result<Vec<MissionContentRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {CONTENT_COLUMNS} FROM mission_contents WHERE mission_id = ?1 \
                     ORDER BY position, timestamp, resource_id"
                ))?;

                statement
                    .query_map([mission_id], MissionContentRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Unfiles every resource holding a hash, reporting their ids.
    ///
    /// A hash is not unique — the same photograph attached to two map items is
    /// two resource rows over one blob — so detaching by hash detaches all of
    /// them, which is what a client asking to remove "that file" means.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn remove_content_by_hash(
        &self,
        mission_id: i64,
        hash: String,
    ) -> Result<Vec<i64>, Error> {
        self.db
            .write(move |tx| {
                let mut statement = tx.prepare(
                    "DELETE FROM mission_contents WHERE mission_id = ?1 AND resource_id IN \
                       (SELECT id FROM resources WHERE hash = ?2) RETURNING resource_id",
                )?;

                statement
                    .query_map(rusqlite::params![mission_id, hash], |row| row.get(0))?
                    .collect()
            })
            .await
    }

    /// The missions a map item is filed under.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn missions_with_uid(&self, uid: String) -> Result<Vec<i64>, Error> {
        self.db
            .read(move |c| {
                let mut statement =
                    c.prepare("SELECT mission_id FROM mission_uids WHERE uid = ?1")?;

                statement.query_map([uid], |row| row.get(0))?.collect()
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
    async fn filing_an_item_twice_reports_it_new_only_once() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db).await;
        let item = MissionUidRow::new(id, "ANDROID-1", Utc::now());

        assert!(
            db.mission_contents()
                .upsert_uid(item.clone())
                .await
                .unwrap()
        );
        assert!(!db.mission_contents().upsert_uid(item).await.unwrap());
        assert_eq!(db.mission_contents().uids(id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn refiling_an_item_refreshes_its_cached_details() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db).await;

        db.mission_contents()
            .upsert_uid(MissionUidRow::new(id, "ANDROID-1", Utc::now()))
            .await
            .unwrap();
        db.mission_contents()
            .upsert_uid(MissionUidRow {
                details: Some(serde_json::json!({ "callsign": "ALPHA" })),
                ..MissionUidRow::new(id, "ANDROID-1", Utc::now())
            })
            .await
            .unwrap();

        let filed = db.mission_contents().uids(id).await.unwrap();

        assert_eq!(filed[0].details.as_ref().unwrap()["callsign"], "ALPHA");
    }

    #[tokio::test]
    async fn unfiling_an_item_reports_whether_it_was_there() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db).await;

        db.mission_contents()
            .upsert_uid(MissionUidRow::new(id, "ANDROID-1", Utc::now()))
            .await
            .unwrap();

        assert!(
            db.mission_contents()
                .remove_uid(id, "ANDROID-1".to_string())
                .await
                .unwrap()
        );
        assert!(
            !db.mission_contents()
                .remove_uid(id, "ANDROID-1".to_string())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_uid_can_be_filed_under_several_missions() {
        let db = Database::open_in_memory().await.unwrap();
        let first = mission(&db).await;
        let second = db
            .missions()
            .create(NewMission::new("Bravo", "MISSION_SUBSCRIBER"))
            .await
            .unwrap()
            .id;

        for id in [first, second] {
            db.mission_contents()
                .upsert_uid(MissionUidRow::new(id, "ANDROID-1", Utc::now()))
                .await
                .unwrap();
        }

        let mut found = db
            .mission_contents()
            .missions_with_uid("ANDROID-1".to_string())
            .await
            .unwrap();
        found.sort_unstable();

        assert_eq!(found, vec![first, second]);
    }
}
