//! `services`: the sidecars registered against this server.
//!
//! A service is a user of kind `service` plus the description it registered
//! with. The two are separate rows because the account is what authenticates —
//! it holds the certificate, the token and the channel memberships — while this
//! row is what the control API and the admin UI read, and either can change
//! without the other.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_api::{Capability, ServiceEndpoints, ServiceState};
use rustak_core::prelude::*;

use crate::db::{
    Database,
    row::{Timestamp, bool_col, enum_col, id_col, json_col, opt_json_col, opt_ts, to_json, ts},
};

/// The columns [`ServiceRow::from_row`] expects, in order.
const COLUMNS: &str = "id, name, user_id, display_name, description, version, capabilities, \
                       endpoints, config, status, status_message, last_heartbeat_at, enabled, \
                       created_at, updated_at, metrics";

/// One row of `services`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRow {
    pub id: ServiceId,
    pub name: ServiceName,
    /// The account it authenticates as, always `users.kind = 'service'`.
    pub user_id: UserId,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub capabilities: Vec<Capability>,
    pub endpoints: Option<ServiceEndpoints>,
    /// Per-service configuration the control API hands back at registration.
    pub config: serde_json::Value,
    pub status: ServiceState,
    pub status_message: Option<String>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The latest numbers the service reported with its heartbeat, whatever it
    /// decided those were. Replaced rather than accumulated; `{}` until the
    /// first heartbeat carries any.
    pub metrics: serde_json::Value,
}

impl ServiceRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            name: ServiceName::from_storage(row.get::<_, String>(1)?),
            user_id: id_col(row, 2)?,
            display_name: row.get(3)?,
            description: row.get(4)?,
            version: row.get(5)?,
            capabilities: json_col(row, 6)?,
            endpoints: opt_json_col(row, 7)?,
            config: json_col(row, 8)?,
            status: enum_col(row, 9, ServiceState::parse)?,
            status_message: row.get(10)?,
            last_heartbeat_at: opt_ts(row, 11)?,
            enabled: bool_col(row, 12)?,
            created_at: ts(row, 13)?,
            updated_at: ts(row, 14)?,
            metrics: json_col(row, 15)?,
        })
    }
}

/// A service about to register.
#[derive(Debug, Clone)]
pub struct NewService {
    pub name: ServiceName,
    pub user_id: UserId,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub capabilities: Vec<Capability>,
    pub endpoints: Option<ServiceEndpoints>,
}

impl NewService {
    /// A service that has registered with nothing but its name.
    pub fn new(name: ServiceName, user_id: UserId) -> Self {
        Self {
            name,
            user_id,
            display_name: None,
            description: None,
            version: None,
            capabilities: Vec::new(),
            endpoints: None,
        }
    }
}

/// Reads and writes `services`.
pub struct ServicesRepo<'a> {
    db: &'a Database,
}

impl<'a> ServicesRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Registers a service, or refreshes the registration of one that is
    /// restarting.
    ///
    /// A sidecar re-registers every time it starts, so this has to be an upsert:
    /// the alternative is either a duplicate row or a failed start-up after a
    /// restart. Its configuration and its status survive, because neither is
    /// the sidecar's to reset.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the account does
    /// not exist or already belongs to another service.
    pub async fn register(&self, new: NewService) -> Result<ServiceRow, Error> {
        self.db
            .write(move |tx| {
                let now = Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO services \
                           (name, user_id, display_name, description, version, capabilities, \
                            endpoints, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
                         ON CONFLICT (name) DO UPDATE SET \
                           user_id = excluded.user_id, \
                           display_name = excluded.display_name, \
                           description = excluded.description, \
                           version = excluded.version, \
                           capabilities = excluded.capabilities, \
                           endpoints = excluded.endpoints, \
                           updated_at = excluded.updated_at \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.name.as_str(),
                        new.user_id.get(),
                        new.display_name,
                        new.description,
                        new.version,
                        to_json(&new.capabilities)?,
                        new.endpoints.map(|e| to_json(&e)).transpose()?,
                        now,
                    ],
                    ServiceRow::from_row,
                )
            })
            .await
    }

    /// Reads one service by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: ServiceId) -> Result<Option<ServiceRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM services WHERE id = ?1"),
                    [id.get()],
                    ServiceRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads one service by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_name(&self, name: &ServiceName) -> Result<Option<ServiceRow>, Error> {
        let name = name.as_str().to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM services WHERE name = ?1"),
                    [name],
                    ServiceRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Every registered service, by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self) -> Result<Vec<ServiceRow>, Error> {
        self.db
            .read(|c| {
                let mut statement =
                    c.prepare(&format!("SELECT {COLUMNS} FROM services ORDER BY name ASC"))?;

                statement.query_map([], ServiceRow::from_row)?.collect()
            })
            .await
    }

    /// Records a heartbeat.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn record_heartbeat(
        &self,
        id: ServiceId,
        state: ServiceState,
        message: Option<String>,
        metrics: serde_json::Value,
    ) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE services \
                     SET status = ?2, status_message = ?3, last_heartbeat_at = ?4, metrics = ?5 \
                     WHERE id = ?1",
                    rusqlite::params![
                        id.get(),
                        state.as_str(),
                        message,
                        Timestamp::now(),
                        to_json(&metrics)?,
                    ],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Marks a service as no longer reporting.
    ///
    /// Called by the sweep that decides a heartbeat is overdue, rather than by
    /// the service itself, which by definition is not talking to us.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn mark_silent_before(&self, cutoff: DateTime<Utc>) -> Result<usize, Error> {
        let cutoff = Timestamp::from(cutoff);

        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE services SET status = 'unknown', status_message = NULL \
                     WHERE status != 'unknown' \
                       AND (last_heartbeat_at IS NULL OR last_heartbeat_at < ?1)",
                    [cutoff],
                )
            })
            .await
    }

    /// Replaces a service's configuration.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_config(
        &self,
        id: ServiceId,
        config: serde_json::Value,
    ) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE services SET config = ?2, updated_at = ?3 WHERE id = ?1",
                    rusqlite::params![id.get(), to_json(&config)?, Timestamp::now()],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Enables or disables a service.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_enabled(&self, id: ServiceId, enabled: bool) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE services SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                    rusqlite::params![id.get(), i64::from(enabled), Timestamp::now()],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Removes a service's registration, leaving its account alone.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, id: ServiceId) -> Result<bool, Error> {
        let deleted = self
            .db
            .write(move |tx| tx.execute("DELETE FROM services WHERE id = ?1", [id.get()]))
            .await?;

        Ok(deleted > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    async fn fixture() -> (Database, UserId) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::service(Username::parse("svc.weather").unwrap()))
            .await
            .unwrap();

        (db, user.id)
    }

    fn service(user_id: UserId) -> NewService {
        NewService {
            version: Some("1.2.3".into()),
            capabilities: vec![Capability::parse("cot.publish").unwrap()],
            ..NewService::new(ServiceName::parse("weather").unwrap(), user_id)
        }
    }

    #[tokio::test]
    async fn a_service_reads_back_as_registered() {
        let (db, user) = fixture().await;

        let registered = db.services().register(service(user)).await.unwrap();

        assert_eq!(registered.name.as_str(), "weather");
        assert_eq!(registered.status, ServiceState::Unknown);
        assert_eq!(registered.version.as_deref(), Some("1.2.3"));
        assert_eq!(registered.capabilities.len(), 1);
        assert_eq!(registered.config, serde_json::json!({}));
        assert_eq!(registered.metrics, serde_json::json!({}));
        assert!(registered.enabled);
        assert_eq!(
            db.services().get(registered.id).await.unwrap().unwrap(),
            registered
        );
    }

    #[tokio::test]
    async fn re_registering_keeps_the_row_its_config_and_its_status() {
        let (db, user) = fixture().await;
        let first = db.services().register(service(user)).await.unwrap();
        db.services()
            .set_config(first.id, serde_json::json!({ "interval": 60 }))
            .await
            .unwrap();
        db.services()
            .record_heartbeat(first.id, ServiceState::Healthy, None, serde_json::json!({}))
            .await
            .unwrap();

        let again = db
            .services()
            .register(NewService {
                version: Some("1.3.0".into()),
                ..service(user)
            })
            .await
            .unwrap();

        assert_eq!(again.id, first.id);
        assert_eq!(again.version.as_deref(), Some("1.3.0"));
        assert_eq!(again.config, serde_json::json!({ "interval": 60 }));
        assert_eq!(again.status, ServiceState::Healthy);
        assert_eq!(db.services().list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_account_backs_one_service() {
        let (db, user) = fixture().await;
        db.services().register(service(user)).await.unwrap();

        let clash = db
            .services()
            .register(NewService::new(ServiceName::parse("other").unwrap(), user))
            .await;

        assert!(clash.is_err(), "the account is already a service's");
    }

    #[tokio::test]
    async fn a_heartbeat_carries_the_services_own_words() {
        let (db, user) = fixture().await;
        let registered = db.services().register(service(user)).await.unwrap();

        db.services()
            .record_heartbeat(
                registered.id,
                ServiceState::Degraded,
                Some("Upstream feed is slow.".into()),
                serde_json::json!({ "queue_depth": 3 }),
            )
            .await
            .unwrap();

        let read = db.services().get(registered.id).await.unwrap().unwrap();
        assert_eq!(read.status, ServiceState::Degraded);
        assert_eq!(
            read.status_message.as_deref(),
            Some("Upstream feed is slow.")
        );
        assert_eq!(read.metrics, serde_json::json!({ "queue_depth": 3 }));
        assert!(read.last_heartbeat_at.is_some());
    }

    #[tokio::test]
    async fn a_service_that_stopped_reporting_falls_back_to_unknown() {
        let (db, user) = fixture().await;
        let registered = db.services().register(service(user)).await.unwrap();
        db.services()
            .record_heartbeat(
                registered.id,
                ServiceState::Healthy,
                Some("Fine.".into()),
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let swept = db
            .services()
            .mark_silent_before(Utc::now() + chrono::TimeDelta::seconds(1))
            .await
            .unwrap();

        assert_eq!(swept, 1);
        let read = db.services().get(registered.id).await.unwrap().unwrap();
        assert_eq!(read.status, ServiceState::Unknown);
        assert_eq!(read.status_message, None);

        // Nothing left to sweep.
        assert_eq!(
            db.services()
                .mark_silent_before(Utc::now() + chrono::TimeDelta::seconds(1))
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_service_can_be_disabled_and_removed() {
        let (db, user) = fixture().await;
        let registered = db.services().register(service(user)).await.unwrap();

        assert!(
            db.services()
                .set_enabled(registered.id, false)
                .await
                .unwrap()
        );
        assert!(
            !db.services()
                .get(registered.id)
                .await
                .unwrap()
                .unwrap()
                .enabled
        );

        assert!(db.services().delete(registered.id).await.unwrap());
        assert!(
            db.users().get(user).await.unwrap().is_some(),
            "the account stays"
        );
    }

    #[tokio::test]
    async fn deleting_the_account_takes_the_registration_with_it() {
        let (db, user) = fixture().await;
        db.services().register(service(user)).await.unwrap();

        db.users().delete(user).await.unwrap();

        assert!(db.services().list().await.unwrap().is_empty());
    }
}
