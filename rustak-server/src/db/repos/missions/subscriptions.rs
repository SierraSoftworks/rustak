//! `mission_subscriptions`: who is watching a mission, and with what role.
//!
//! A subscription is keyed by `(mission, clientUid)` rather than by account,
//! because the thing that subscribes is a *device*: one person's phone and
//! laptop are two subscriptions with two tokens, and revoking one must not
//! revoke the other.
//!
//! # The subscription uid is the token's identity
//!
//! `subscription_uid` is the value the `SUBSCRIPTION` claim carries, so a
//! mission token is resolved by looking one up here. Re-subscribing mints a new
//! uid — which is what retires the previous token — while keeping the role the
//! subscription already had, because a device reconnecting is not a request to
//! be demoted to the default.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{Database, row::Timestamp, row::ts};

/// Every column [`MissionSubscriptionRow::from_row`] reads, in order.
const COLUMNS: &str = "id, mission_id, subscription_uid, client_uid, username, role, token_jti, \
     created_at";

/// One stored subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionSubscriptionRow {
    pub id: i64,
    pub mission_id: i64,
    /// The value the token's `SUBSCRIPTION` claim carries.
    pub subscription_uid: String,
    pub client_uid: String,
    pub username: Option<String>,
    /// The stored spelling of the subscriber's role.
    pub role: String,
    /// The `jti` of the token last minted for this subscription.
    pub token_jti: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl MissionSubscriptionRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            mission_id: row.get(1)?,
            subscription_uid: row.get(2)?,
            client_uid: row.get(3)?,
            username: row.get(4)?,
            role: row.get(5)?,
            token_jti: row.get(6)?,
            created_at: ts(row, 7)?,
        })
    }
}

/// What a subscribe supplies.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSubscription {
    pub mission_id: i64,
    pub subscription_uid: String,
    pub client_uid: String,
    pub username: Option<String>,
    /// Used only when the subscription is new; an existing one keeps its role.
    pub role: String,
    pub token_jti: Option<String>,
}

/// Reads and writes `mission_subscriptions`.
pub struct MissionSubscriptionsRepo<'a> {
    db: &'a Database,
}

impl<'a> MissionSubscriptionsRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Subscribes a client, keeping the role an existing subscription has.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn upsert(&self, new: NewSubscription) -> Result<MissionSubscriptionRow, Error> {
        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO mission_subscriptions \
                           (mission_id, subscription_uid, client_uid, username, role, token_jti, \
                            created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT (mission_id, client_uid) DO UPDATE SET \
                           subscription_uid = excluded.subscription_uid, \
                           username = excluded.username, token_jti = excluded.token_jti \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.mission_id,
                        new.subscription_uid,
                        new.client_uid,
                        new.username,
                        new.role,
                        new.token_jti,
                        Timestamp::now(),
                    ],
                    MissionSubscriptionRow::from_row,
                )
            })
            .await
    }

    /// One client's subscription to a mission.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_client(
        &self,
        mission_id: i64,
        client_uid: String,
    ) -> Result<Option<MissionSubscriptionRow>, Error> {
        self.db
            .read(move |c| {
                c.query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM mission_subscriptions \
                         WHERE mission_id = ?1 AND client_uid = ?2"
                    ),
                    rusqlite::params![mission_id, client_uid],
                    MissionSubscriptionRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// The subscription a `SUBSCRIPTION` token names.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_subscription_uid(
        &self,
        subscription_uid: String,
    ) -> Result<Option<MissionSubscriptionRow>, Error> {
        self.db
            .read(move |c| {
                c.query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM mission_subscriptions WHERE subscription_uid = ?1"
                    ),
                    [subscription_uid],
                    MissionSubscriptionRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Every subscription to a mission, oldest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, mission_id: i64) -> Result<Vec<MissionSubscriptionRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_subscriptions WHERE mission_id = ?1 \
                     ORDER BY created_at, id"
                ))?;

                statement
                    .query_map([mission_id], MissionSubscriptionRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Every subscription this installation holds, for the admin listings.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn all(&self) -> Result<Vec<MissionSubscriptionRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_subscriptions ORDER BY mission_id, id"
                ))?;

                statement
                    .query_map([], MissionSubscriptionRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Unsubscribes a client, reporting whether it was subscribed.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, mission_id: i64, client_uid: String) -> Result<bool, Error> {
        self.db
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM mission_subscriptions WHERE mission_id = ?1 AND client_uid = ?2",
                    rusqlite::params![mission_id, client_uid],
                )? > 0)
            })
            .await
    }

    /// Sets the role of one client's subscription, or of every subscription an
    /// account holds, reporting how many rows changed.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_role(
        &self,
        mission_id: i64,
        client_uid: Option<String>,
        username: Option<String>,
        role: String,
    ) -> Result<usize, Error> {
        self.db
            .write(move |tx| match (client_uid, username) {
                (Some(client_uid), _) => tx.execute(
                    "UPDATE mission_subscriptions SET role = ?3 \
                         WHERE mission_id = ?1 AND client_uid = ?2",
                    rusqlite::params![mission_id, client_uid, role],
                ),
                (None, Some(username)) => tx.execute(
                    "UPDATE mission_subscriptions SET role = ?3 \
                         WHERE mission_id = ?1 AND username = ?2 COLLATE NOCASE",
                    rusqlite::params![mission_id, username, role],
                ),
                (None, None) => Ok(0),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::repos::missions::NewMission;

    use super::*;

    async fn mission(db: &Database, name: &str) -> i64 {
        db.missions()
            .create(NewMission::new(name, "MISSION_SUBSCRIBER"))
            .await
            .unwrap()
            .id
    }

    fn subscription(mission_id: i64, uid: &str) -> NewSubscription {
        NewSubscription {
            mission_id,
            subscription_uid: format!("sub-{uid}"),
            client_uid: uid.to_string(),
            username: Some("j.smith".to_string()),
            role: "MISSION_SUBSCRIBER".to_string(),
            token_jti: None,
        }
    }

    #[tokio::test]
    async fn resubscribing_keeps_the_role_and_mints_a_new_identity() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db, "Alpha").await;

        db.mission_subscriptions()
            .upsert(subscription(id, "ANDROID-1"))
            .await
            .unwrap();
        db.mission_subscriptions()
            .set_role(
                id,
                Some("ANDROID-1".to_string()),
                None,
                "MISSION_OWNER".to_string(),
            )
            .await
            .unwrap();

        let again = db
            .mission_subscriptions()
            .upsert(NewSubscription {
                subscription_uid: "sub-second".to_string(),
                role: "MISSION_READONLY_SUBSCRIBER".to_string(),
                ..subscription(id, "ANDROID-1")
            })
            .await
            .unwrap();

        assert_eq!(again.role, "MISSION_OWNER", "the stored role wins");
        assert_eq!(again.subscription_uid, "sub-second");
        assert_eq!(db.mission_subscriptions().list(id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_subscription_is_found_by_the_uid_its_token_carries() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db, "Alpha").await;

        db.mission_subscriptions()
            .upsert(subscription(id, "ANDROID-1"))
            .await
            .unwrap();

        let found = db
            .mission_subscriptions()
            .by_subscription_uid("sub-ANDROID-1".to_string())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(found.client_uid, "ANDROID-1");
    }

    #[tokio::test]
    async fn setting_a_role_by_username_reaches_every_device() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db, "Alpha").await;

        for uid in ["ANDROID-1", "ANDROID-2"] {
            db.mission_subscriptions()
                .upsert(subscription(id, uid))
                .await
                .unwrap();
        }

        let changed = db
            .mission_subscriptions()
            .set_role(
                id,
                None,
                Some("J.SMITH".to_string()),
                "MISSION_OWNER".to_string(),
            )
            .await
            .unwrap();

        assert_eq!(changed, 2);
    }

    #[tokio::test]
    async fn unsubscribing_reports_whether_there_was_a_subscription() {
        let db = Database::open_in_memory().await.unwrap();
        let id = mission(&db, "Alpha").await;

        db.mission_subscriptions()
            .upsert(subscription(id, "ANDROID-1"))
            .await
            .unwrap();

        assert!(
            db.mission_subscriptions()
                .delete(id, "ANDROID-1".to_string())
                .await
                .unwrap()
        );
        assert!(
            !db.mission_subscriptions()
                .delete(id, "ANDROID-1".to_string())
                .await
                .unwrap()
        );
    }
}
