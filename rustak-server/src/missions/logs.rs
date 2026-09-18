//! Mission log entries: the written record beside the map.
//!
//! A log entry is free text, optionally dated (`dtg`), optionally tied to a map
//! item (`entryUid`) and to uploaded files (`contentHashes`), and it may belong
//! to **several** missions at once — ATAK writes one entry naming every mission
//! the operator had open.
//!
//! # One row per mission, sharing an identifier
//!
//! `mission_logs` keys on the mission, so an entry naming three missions is
//! three rows with the same `log_id`. Reading one back unions the mission names
//! again, which is what `missionNames` carries. The alternative — a join table
//! — would buy nothing: every read of a log entry is already scoped to either
//! one mission or one identifier.
//!
//! # `servertime`, not `serverTime`
//!
//! A `MissionChange` spells it with a capital T and a log entry does not. Both
//! are real and neither is ours to unify (`compat/missions.md` Gotchas).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::row::{Timestamp, opt_ts, ts};
use crate::marti::{MartiError, time};

use super::model::Mission;
use super::service::MissionService;

/// Every column [`LogEntry::from_row`] reads, in order.
const COLUMNS: &str = "log_id, content, creator_uid, entry_uid, content_hashes, keywords, \
     servertime, dtg, created_at";

/// One log entry, as the service reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub id: String,
    pub content: String,
    pub creator_uid: Option<String>,
    pub entry_uid: Option<String>,
    /// Every mission this entry was written to, in the order they were named.
    pub mission_names: Vec<String>,
    pub servertime: DateTime<Utc>,
    pub dtg: Option<DateTime<Utc>>,
    pub created: DateTime<Utc>,
    pub content_hashes: Vec<String>,
    pub keywords: Vec<String>,
}

impl LogEntry {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            content: row.get(1)?,
            creator_uid: row.get(2)?,
            entry_uid: row.get(3)?,
            mission_names: Vec::new(),
            content_hashes: crate::db::row::json_col(row, 4)?,
            keywords: crate::db::row::json_col(row, 5)?,
            servertime: ts(row, 6)?,
            dtg: opt_ts(row, 7)?,
            created: ts(row, 8)?,
        })
    }

    /// The wire shape.
    pub fn to_json(&self) -> LogEntryJson {
        LogEntryJson {
            id: Some(self.id.clone()),
            content: self.content.clone(),
            creator_uid: self.creator_uid.clone(),
            entry_uid: self.entry_uid.clone(),
            mission_names: self.mission_names.clone(),
            servertime: Some(time::cot_date(self.servertime)),
            dtg: self.dtg.map(time::cot_date),
            created: Some(time::cot_date(self.created)),
            content_hashes: self.content_hashes.clone(),
            keywords: self.keywords.clone(),
        }
    }
}

/// A log entry as a client writes and reads it.
///
/// `id` is absent on a `POST` and required on a `PUT`; `servertime` is the
/// other way round — the server assigns it, so a `PUT` carrying one is refused
/// rather than having it silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogEntryJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(
        rename = "creatorUid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub creator_uid: Option<String>,
    #[serde(rename = "entryUid", default, skip_serializing_if = "Option::is_none")]
    pub entry_uid: Option<String>,
    #[serde(rename = "missionNames", default)]
    pub mission_names: Vec<String>,
    /// Lower-case `t`, unlike `MissionChange.serverTime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servertime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(rename = "contentHashes", default)]
    pub content_hashes: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

impl MissionService {
    /// Writes one entry against every mission it names.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a body naming no missions, and a
    /// system error if a write fails.
    pub async fn write_log(
        &self,
        id: &str,
        body: &LogEntryJson,
        missions: &[Mission],
    ) -> Result<LogEntry, MartiError> {
        if missions.is_empty() {
            return Err(MartiError::InvalidRequest(
                "a log entry has to name at least one mission".to_string(),
            ));
        }

        let now = Utc::now();
        let dtg = match &body.dtg {
            Some(value) => Some(time::parse_date(value)?),
            None => Some(now),
        };

        // Rewritten rather than merged: an update replaces the entry, and
        // deleting first keeps a mission dropped from `missionNames` from
        // keeping a stale copy.
        self.delete_log(id).await?;

        for mission in missions {
            let row = StoredLog {
                log_id: id.to_string(),
                mission_id: mission.id,
                content: body.content.clone(),
                creator_uid: body.creator_uid.clone(),
                entry_uid: body.entry_uid.clone(),
                content_hashes: body.content_hashes.clone(),
                keywords: body.keywords.clone(),
                at: now,
                dtg,
            };

            self.insert_log(row).await?;
        }

        Ok(LogEntry {
            id: id.to_string(),
            content: body.content.clone(),
            creator_uid: body.creator_uid.clone(),
            entry_uid: body.entry_uid.clone(),
            mission_names: missions.iter().map(|m| m.name.clone()).collect(),
            servertime: now,
            dtg,
            created: now,
            content_hashes: body.content_hashes.clone(),
            keywords: body.keywords.clone(),
        })
    }

    /// One log entry by identifier, with every mission it was written to.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub async fn log_entry(&self, id: &str) -> Result<Option<LogEntry>, MartiError> {
        let id = id.to_string();
        let rows: Vec<(LogEntry, i64)> = self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS}, mission_id FROM mission_logs WHERE log_id = ?1 ORDER BY id"
                ))?;

                statement
                    .query_map(rusqlite::params![id], |row| {
                        Ok((LogEntry::from_row(row)?, row.get(9)?))
                    })?
                    .collect()
            })
            .await?;

        let Some((mut entry, _)) = rows.first().cloned() else {
            return Ok(None);
        };

        for (_, mission_id) in &rows {
            if let Some(mission) = self.db().missions().by_id(*mission_id).await? {
                entry.mission_names.push(mission.name);
            }
        }

        Ok(Some(entry))
    }

    /// Removes every row of an entry, reporting whether there were any.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn delete_log(&self, id: &str) -> Result<bool, MartiError> {
        let id = id.to_string();

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM mission_logs WHERE log_id = ?1",
                    rusqlite::params![id],
                )? > 0)
            })
            .await?)
    }

    /// One mission's log entries, newest first, inside a window.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub async fn mission_logs(
        &self,
        mission: &Mission,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<LogEntry>, MartiError> {
        let mission_id = mission.id;
        let name = mission.name.clone();

        let mut entries: Vec<LogEntry> = self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_logs \
                     WHERE mission_id = ?1 AND created_at >= ?2 AND created_at <= ?3 \
                     ORDER BY created_at DESC, id DESC"
                ))?;

                statement
                    .query_map(
                        rusqlite::params![mission_id, Timestamp::from(start), Timestamp::from(end)],
                        LogEntry::from_row,
                    )?
                    .collect()
            })
            .await?;

        for entry in &mut entries {
            entry.mission_names.push(name.clone());
        }

        Ok(entries)
    }

    /// Every log entry on the server, for the administrative listing.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub async fn all_logs(&self) -> Result<Vec<LogEntry>, MartiError> {
        let ids: Vec<String> = self
            .db()
            .read(|c| {
                let mut statement =
                    c.prepare("SELECT DISTINCT log_id FROM mission_logs ORDER BY log_id")?;

                statement.query_map([], |row| row.get(0))?.collect()
            })
            .await?;

        let mut entries = Vec::with_capacity(ids.len());

        for id in ids {
            if let Some(entry) = self.log_entry(&id).await? {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    /// Stores one row of an entry.
    async fn insert_log(&self, row: StoredLog) -> Result<(), MartiError> {
        let hashes = serde_json::to_string(&row.content_hashes)?;
        let keywords = serde_json::to_string(&row.keywords)?;

        self.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mission_logs \
                       (log_id, mission_id, content, creator_uid, entry_uid, content_hashes, \
                        keywords, servertime, dtg, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?8)",
                    rusqlite::params![
                        row.log_id,
                        row.mission_id,
                        row.content,
                        row.creator_uid,
                        row.entry_uid,
                        hashes,
                        keywords,
                        Timestamp::from(row.at),
                        row.dtg.map(Timestamp::from),
                    ],
                )
            })
            .await?;

        Ok(())
    }
}

/// One row about to be written.
struct StoredLog {
    log_id: String,
    mission_id: i64,
    content: String,
    creator_uid: Option<String>,
    entry_uid: Option<String>,
    content_hashes: Vec<String>,
    keywords: Vec<String>,
    at: DateTime<Utc>,
    dtg: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    use super::super::roles::Role;
    use super::*;

    async fn fixture() -> (AppContext, MissionService, Mission, Mission) {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let service = MissionService::new(context.clone());
        let mut made = Vec::new();

        for name in ["Kettle", "Anvil"] {
            let row = context
                .db()
                .missions()
                .create(crate::db::repos::NewMission::new(
                    name,
                    Role::Subscriber.as_str(),
                ))
                .await
                .unwrap();

            made.push(Mission::from_row(row));
        }

        let anvil = made.pop().unwrap();
        let kettle = made.pop().unwrap();

        (context, service, kettle, anvil)
    }

    fn body(content: &str) -> LogEntryJson {
        LogEntryJson {
            content: content.to_string(),
            creator_uid: Some("ANDROID-1".to_string()),
            keywords: vec!["sitrep".to_string()],
            ..LogEntryJson::default()
        }
    }

    #[tokio::test]
    async fn an_entry_naming_two_missions_is_readable_from_either_and_names_both() {
        let (_context, service, kettle, anvil) = fixture().await;

        let written = service
            .write_log(
                "log-1",
                &body("first light"),
                &[kettle.clone(), anvil.clone()],
            )
            .await
            .unwrap();

        assert_eq!(written.mission_names, vec!["Kettle", "Anvil"]);
        let read = service.log_entry("log-1").await.unwrap().unwrap();
        assert_eq!(read.content, "first light");
        assert_eq!(read.mission_names.len(), 2);
        assert_eq!(read.keywords, vec!["sitrep"]);
        assert_eq!(read.to_json().id.as_deref(), Some("log-1"));
    }

    #[tokio::test]
    async fn rewriting_an_entry_with_fewer_missions_drops_the_one_it_left_out() {
        // A merge would leave the dropped mission holding a stale copy, which
        // is what makes an edit look like it did not take.
        let (_context, service, kettle, anvil) = fixture().await;

        service
            .write_log("log-1", &body("first"), &[kettle.clone(), anvil.clone()])
            .await
            .unwrap();
        service
            .write_log("log-1", &body("second"), std::slice::from_ref(&kettle))
            .await
            .unwrap();

        let read = service.log_entry("log-1").await.unwrap().unwrap();

        assert_eq!(read.content, "second");
        assert_eq!(read.mission_names, vec!["Kettle"]);
        assert!(
            service
                .mission_logs(&anvil, DateTime::UNIX_EPOCH, Utc::now())
                .await
                .unwrap()
                .is_empty(),
        );
    }

    #[tokio::test]
    async fn an_entry_naming_no_mission_is_refused() {
        let (_context, service, _kettle, _anvil) = fixture().await;

        assert!(matches!(
            service.write_log("log-1", &body("nowhere"), &[]).await,
            Err(MartiError::InvalidRequest(_)),
        ));
    }

    #[tokio::test]
    async fn deleting_an_entry_removes_every_row_of_it() {
        let (_context, service, kettle, anvil) = fixture().await;
        service
            .write_log("log-1", &body("first"), &[kettle, anvil])
            .await
            .unwrap();

        assert!(service.delete_log("log-1").await.unwrap());
        assert!(service.log_entry("log-1").await.unwrap().is_none());
        assert!(
            !service.delete_log("log-1").await.unwrap(),
            "deleting it twice reports nothing removed",
        );
        assert!(service.all_logs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_window_that_ended_before_the_entry_is_empty() {
        let (_context, service, kettle, _anvil) = fixture().await;
        service
            .write_log("log-1", &body("first"), std::slice::from_ref(&kettle))
            .await
            .unwrap();

        let before = service
            .mission_logs(
                &kettle,
                DateTime::UNIX_EPOCH,
                Utc::now() - chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert!(before.is_empty());
        assert_eq!(
            service
                .mission_logs(&kettle, DateTime::UNIX_EPOCH, Utc::now())
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
