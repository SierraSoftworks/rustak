//! `settings`: the values the first-run wizard and the admin UI own.
//!
//! This is the *lower* of the two configuration layers. Anything set in the
//! TOML file wins, because an operator who wrote a value into a file they
//! deploy expects it to survive somebody clicking about in the UI. What lives
//! here is what has no file to come from: the server name the wizard asked for,
//! the node id, when setup finished.
//!
//! Values are JSON so that a setting can be a string, a number, a flag or a
//! small object without the table having to know which. A scalar is JSON too —
//! `"rustak"`, not `rustak` — which the `json_valid` check enforces.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    row::{Timestamp, json_col, to_json, ts},
};

/// One row of `settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub key: String,
    /// The value, as stored: valid JSON.
    pub value: serde_json::Value,
    pub updated_at: DateTime<Utc>,
    /// Who last changed it, where a person did.
    pub updated_by: Option<Username>,
}

impl SettingRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            key: row.get(0)?,
            value: json_col(row, 1)?,
            updated_at: ts(row, 2)?,
            updated_by: row.get::<_, Option<String>>(3)?.map(Username::from_storage),
        })
    }
}

/// Reads and writes `settings`.
pub struct SettingsRepo<'a> {
    db: &'a Database,
}

impl<'a> SettingsRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Reads one setting as the type the caller expects.
    ///
    /// A key that is not set and a key whose value is not the shape asked for
    /// are different answers: the first is `None`, the second an error, because
    /// silently falling back would hide a setting the operator thought they had
    /// made.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails or the stored
    /// value is not a `T`.
    pub async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        key: &str,
    ) -> Result<Option<T>, Error> {
        let key = key.to_owned();

        self.db
            .read(move |c| {
                c.query_one("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    json_col(row, 0)
                })
                .optional()
            })
            .await
    }

    /// Reads one setting with everything the UI shows beside it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_row(&self, key: &str) -> Result<Option<SettingRow>, Error> {
        let key = key.to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    "SELECT key, value, updated_at, updated_by FROM settings WHERE key = ?1",
                    [key],
                    SettingRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Writes one setting.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the value cannot be serialised
    /// or the write fails.
    pub async fn set<T: Serialize + Send + 'static>(
        &self,
        key: &str,
        value: T,
        by: Option<&Username>,
    ) -> Result<(), Error> {
        let key = key.to_owned();
        let by = by.map(|by| by.as_str().to_owned());

        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO settings (key, value, updated_at, updated_by) \
                     VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT (key) DO UPDATE SET \
                       value = excluded.value, \
                       updated_at = excluded.updated_at, \
                       updated_by = excluded.updated_by",
                    rusqlite::params![key, to_json(&value)?, Timestamp::now(), by],
                )
            })
            .await?;

        Ok(())
    }

    /// Removes a setting, letting whatever default applies take over again.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn remove(&self, key: &str) -> Result<bool, Error> {
        let key = key.to_owned();

        let removed = self
            .db
            .write(move |tx| tx.execute("DELETE FROM settings WHERE key = ?1", [key]))
            .await?;

        Ok(removed > 0)
    }

    /// Every setting, by key.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn all(&self) -> Result<Vec<SettingRow>, Error> {
        self.db
            .read(|c| {
                let mut statement = c.prepare(
                    "SELECT key, value, updated_at, updated_by FROM settings ORDER BY key ASC",
                )?;

                statement.query_map([], SettingRow::from_row)?.collect()
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn a_setting_reads_back_as_the_type_it_was_written_as() {
        let db = db().await;
        let admin = Username::parse("admin").unwrap();

        db.settings()
            .set("server.name", "rustak", Some(&admin))
            .await
            .unwrap();
        db.settings()
            .set("setup.complete", true, None)
            .await
            .unwrap();
        db.settings()
            .set("server.domains", vec!["tak.example.com"], None)
            .await
            .unwrap();

        assert_eq!(
            db.settings().get::<String>("server.name").await.unwrap(),
            Some("rustak".into())
        );
        assert_eq!(
            db.settings().get::<bool>("setup.complete").await.unwrap(),
            Some(true)
        );
        assert_eq!(
            db.settings()
                .get::<Vec<String>>("server.domains")
                .await
                .unwrap(),
            Some(vec!["tak.example.com".to_string()])
        );
    }

    #[tokio::test]
    async fn a_key_that_was_never_set_is_none() {
        let db = db().await;

        assert_eq!(
            db.settings().get::<String>("server.name").await.unwrap(),
            None
        );
        assert!(
            db.settings()
                .get_row("server.name")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_value_of_the_wrong_shape_is_an_error_rather_than_a_default() {
        let db = db().await;
        db.settings()
            .set("server.name", "rustak", None)
            .await
            .unwrap();

        assert!(db.settings().get::<u32>("server.name").await.is_err());
    }

    #[tokio::test]
    async fn writing_again_replaces_the_value_and_records_who() {
        let db = db().await;
        let admin = Username::parse("admin").unwrap();

        db.settings()
            .set("server.name", "first", None)
            .await
            .unwrap();
        db.settings()
            .set("server.name", "second", Some(&admin))
            .await
            .unwrap();

        let row = db.settings().get_row("server.name").await.unwrap().unwrap();
        assert_eq!(row.value, serde_json::json!("second"));
        assert_eq!(row.updated_by.unwrap().as_str(), "admin");
        assert_eq!(db.settings().all().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_column_refuses_a_value_that_is_not_json() {
        let db = db().await;

        let refused = db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO settings (key, value, updated_at) \
                     VALUES ('server.name', 'rustak', '2026-01-01T00:00:00.000Z')",
                    [],
                )
            })
            .await;

        assert!(refused.is_err(), "a bare scalar is not JSON");
    }

    #[tokio::test]
    async fn removing_a_setting_reports_whether_there_was_one() {
        let db = db().await;
        db.settings()
            .set("server.name", "rustak", None)
            .await
            .unwrap();

        assert!(db.settings().remove("server.name").await.unwrap());
        assert!(!db.settings().remove("server.name").await.unwrap());
        assert!(db.settings().all().await.unwrap().is_empty());
    }
}
