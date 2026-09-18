//! `refresh_tokens`: rotating refresh tokens, grouped into families.
//!
//! A refresh token is spent the moment it is used and replaced with a new one
//! in the same family. If a token that has already been spent turns up again,
//! either the client kept a copy or somebody else has one — and there is no way
//! to tell which, so the whole family is revoked and everybody signs in again.
//! That is the standard answer to refresh-token theft, and it is the reason the
//! `family` column exists rather than a simple `replaced_by` link.
//!
//! The token itself is never stored. Refresh tokens are high-entropy random
//! strings, so sha256 is enough to find the row and argon2 would only make
//! every refresh slow.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    row::{Timestamp, id_col, opt_ts, ts},
};

/// The columns [`RefreshTokenRow::from_row`] expects, in order.
const COLUMNS: &str = "id, user_id, token_hash, family, scope, client, created_at, expires_at, \
                       used_at, revoked_at";

/// One row of `refresh_tokens`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshTokenRow {
    pub id: i64,
    pub user_id: UserId,
    /// Hex sha256 of the token that was handed out.
    pub token_hash: String,
    /// The rotation chain this token belongs to.
    pub family: String,
    pub scope: String,
    /// Which client it was issued to, for the sessions list in the UI.
    pub client: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Set when the token was exchanged. A second exchange is a replay.
    pub used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl RefreshTokenRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            user_id: id_col(row, 1)?,
            token_hash: row.get(2)?,
            family: row.get(3)?,
            scope: row.get(4)?,
            client: row.get(5)?,
            created_at: ts(row, 6)?,
            expires_at: ts(row, 7)?,
            used_at: opt_ts(row, 8)?,
            revoked_at: opt_ts(row, 9)?,
        })
    }

    /// Whether this token may be exchanged at `now`.
    pub fn is_usable_at(&self, now: DateTime<Utc>) -> bool {
        self.used_at.is_none() && self.revoked_at.is_none() && self.expires_at > now
    }
}

/// A refresh token about to be issued.
#[derive(Debug, Clone)]
pub struct NewRefreshToken {
    pub user_id: UserId,
    pub token_hash: String,
    /// Reuse the family of the token being rotated; a fresh sign-in starts one.
    pub family: String,
    pub scope: String,
    pub client: Option<String>,
    pub expires_at: DateTime<Utc>,
}

/// What an exchange found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exchange {
    /// The token was live and has now been spent.
    Spent(Box<RefreshTokenRow>),
    /// The token was spent already, or revoked: its family has been revoked.
    Replayed { family: String, revoked: usize },
    /// No such token, or it had expired.
    Unknown,
}

/// Reads and writes `refresh_tokens`.
pub struct RefreshTokensRepo<'a> {
    db: &'a Database,
}

impl<'a> RefreshTokensRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Issues a refresh token.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the user does not
    /// exist or the hash is somehow already stored.
    pub async fn create(&self, new: NewRefreshToken) -> Result<RefreshTokenRow, Error> {
        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO refresh_tokens \
                           (user_id, token_hash, family, scope, client, created_at, expires_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.user_id.get(),
                        new.token_hash,
                        new.family,
                        new.scope,
                        new.client,
                        Timestamp::now(),
                        Timestamp::from(new.expires_at),
                    ],
                    RefreshTokenRow::from_row,
                )
            })
            .await
    }

    /// Reads a token by the hash of the string a client presented.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_hash(&self, token_hash: &str) -> Result<Option<RefreshTokenRow>, Error> {
        let token_hash = token_hash.to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM refresh_tokens WHERE token_hash = ?1"),
                    [token_hash],
                    RefreshTokenRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Spends a token, or reports a replay and revokes its family.
    ///
    /// Both halves happen in one transaction, so two simultaneous exchanges of
    /// the same token cannot both succeed — which is exactly the race a thief
    /// racing the real client would rely on.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn exchange(&self, token_hash: &str) -> Result<Exchange, Error> {
        let token_hash = token_hash.to_owned();

        self.db
            .write(move |tx| {
                let now = Timestamp::now();
                let Some(row) = tx
                    .query_one(
                        &format!("SELECT {COLUMNS} FROM refresh_tokens WHERE token_hash = ?1"),
                        [&token_hash],
                        RefreshTokenRow::from_row,
                    )
                    .optional()?
                else {
                    return Ok(Exchange::Unknown);
                };

                if row.used_at.is_some() || row.revoked_at.is_some() {
                    let revoked = tx.execute(
                        "UPDATE refresh_tokens SET revoked_at = ?2 \
                         WHERE family = ?1 AND revoked_at IS NULL",
                        rusqlite::params![&row.family, now],
                    )?;

                    return Ok(Exchange::Replayed {
                        family: row.family,
                        revoked,
                    });
                }

                if row.expires_at <= now.get() {
                    return Ok(Exchange::Unknown);
                }

                tx.execute(
                    "UPDATE refresh_tokens SET used_at = ?2 WHERE id = ?1",
                    rusqlite::params![row.id, now],
                )?;

                Ok(Exchange::Spent(Box::new(row)))
            })
            .await
    }

    /// Revokes every token in one family: a sign-out, or a detected replay.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke_family(&self, family: &str) -> Result<usize, Error> {
        let family = family.to_owned();

        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE refresh_tokens SET revoked_at = ?2 \
                     WHERE family = ?1 AND revoked_at IS NULL",
                    rusqlite::params![family, Timestamp::now()],
                )
            })
            .await
    }

    /// Revokes every token a user holds: sign out everywhere.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke_all_for_user(&self, user_id: UserId) -> Result<usize, Error> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE refresh_tokens SET revoked_at = ?2 \
                     WHERE user_id = ?1 AND revoked_at IS NULL",
                    rusqlite::params![user_id.get(), Timestamp::now()],
                )
            })
            .await
    }

    /// Every live token a user holds, for the sessions list.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_live_for_user(&self, user_id: UserId) -> Result<Vec<RefreshTokenRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM refresh_tokens \
                     WHERE user_id = ?1 AND used_at IS NULL AND revoked_at IS NULL \
                       AND expires_at > ?2 \
                     ORDER BY created_at DESC"
                ))?;
                let rows = statement.query_map(
                    rusqlite::params![user_id.get(), Timestamp::now()],
                    RefreshTokenRow::from_row,
                )?;

                rows.collect()
            })
            .await
    }

    /// Deletes tokens that expired before `before`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn prune(&self, before: DateTime<Utc>) -> Result<usize, Error> {
        let before = Timestamp::from(before);

        self.db
            .write(move |tx| {
                tx.execute("DELETE FROM refresh_tokens WHERE expires_at < ?1", [before])
            })
            .await
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
            .create(NewUser::person(Username::parse("j.smith").unwrap()))
            .await
            .unwrap();

        (db, user.id)
    }

    fn issued(user_id: UserId, hash: &str, family: &str) -> NewRefreshToken {
        NewRefreshToken {
            user_id,
            token_hash: hash.into(),
            family: family.into(),
            scope: "admin".into(),
            client: Some("rustak-ui".into()),
            expires_at: Utc::now() + chrono::TimeDelta::days(30),
        }
    }

    #[tokio::test]
    async fn a_token_reads_back_as_issued() {
        let (db, user) = fixture().await;

        let created = db
            .refresh_tokens()
            .create(issued(user, "h1", "f1"))
            .await
            .unwrap();

        assert!(created.is_usable_at(Utc::now()));
        assert_eq!(created.client.as_deref(), Some("rustak-ui"));
        assert_eq!(
            db.refresh_tokens()
                .get_by_hash("h1")
                .await
                .unwrap()
                .unwrap(),
            created
        );
    }

    #[tokio::test]
    async fn a_hash_belongs_to_one_token() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(issued(user, "h1", "f1"))
            .await
            .unwrap();

        assert!(
            db.refresh_tokens()
                .create(issued(user, "h1", "f2"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn exchanging_spends_the_token() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(issued(user, "h1", "f1"))
            .await
            .unwrap();

        let Exchange::Spent(spent) = db.refresh_tokens().exchange("h1").await.unwrap() else {
            panic!("the first exchange should succeed");
        };

        assert_eq!(spent.family, "f1");
        assert!(
            db.refresh_tokens()
                .get_by_hash("h1")
                .await
                .unwrap()
                .unwrap()
                .used_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_replayed_token_revokes_its_whole_family() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(issued(user, "h1", "f1"))
            .await
            .unwrap();
        db.refresh_tokens().exchange("h1").await.unwrap();
        // The rotation that followed it.
        db.refresh_tokens()
            .create(issued(user, "h2", "f1"))
            .await
            .unwrap();
        // Somebody else's session, which must survive.
        db.refresh_tokens()
            .create(issued(user, "h3", "f2"))
            .await
            .unwrap();

        let replayed = db.refresh_tokens().exchange("h1").await.unwrap();

        assert_eq!(
            replayed,
            Exchange::Replayed {
                family: "f1".into(),
                revoked: 2,
            }
        );
        assert!(
            !db.refresh_tokens()
                .get_by_hash("h2")
                .await
                .unwrap()
                .unwrap()
                .is_usable_at(Utc::now())
        );
        assert!(
            db.refresh_tokens()
                .get_by_hash("h3")
                .await
                .unwrap()
                .unwrap()
                .is_usable_at(Utc::now())
        );
    }

    #[tokio::test]
    async fn an_unknown_or_expired_token_is_not_a_replay() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(NewRefreshToken {
                expires_at: Utc::now() - chrono::TimeDelta::hours(1),
                ..issued(user, "old", "f1")
            })
            .await
            .unwrap();

        assert_eq!(
            db.refresh_tokens().exchange("nope").await.unwrap(),
            Exchange::Unknown
        );
        assert_eq!(
            db.refresh_tokens().exchange("old").await.unwrap(),
            Exchange::Unknown
        );
    }

    #[tokio::test]
    async fn signing_out_revokes_a_family_or_everything() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(issued(user, "h1", "f1"))
            .await
            .unwrap();
        db.refresh_tokens()
            .create(issued(user, "h2", "f2"))
            .await
            .unwrap();

        assert_eq!(db.refresh_tokens().revoke_family("f1").await.unwrap(), 1);
        assert_eq!(
            db.refresh_tokens()
                .list_live_for_user(user)
                .await
                .unwrap()
                .len(),
            1
        );

        assert_eq!(
            db.refresh_tokens().revoke_all_for_user(user).await.unwrap(),
            1
        );
        assert!(
            db.refresh_tokens()
                .list_live_for_user(user)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pruning_removes_the_expired_and_keeps_the_live() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(NewRefreshToken {
                expires_at: Utc::now() - chrono::TimeDelta::days(1),
                ..issued(user, "old", "f1")
            })
            .await
            .unwrap();
        db.refresh_tokens()
            .create(issued(user, "live", "f2"))
            .await
            .unwrap();

        assert_eq!(db.refresh_tokens().prune(Utc::now()).await.unwrap(), 1);
        assert!(
            db.refresh_tokens()
                .get_by_hash("live")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn deleting_a_user_takes_their_sessions_with_them() {
        let (db, user) = fixture().await;
        db.refresh_tokens()
            .create(issued(user, "h1", "f1"))
            .await
            .unwrap();

        db.users().delete(user).await.unwrap();

        assert!(
            db.refresh_tokens()
                .get_by_hash("h1")
                .await
                .unwrap()
                .is_none()
        );
    }
}
