//! `revoked_jtis`: the access tokens we have disowned before they expired.
//!
//! An access token is a signed bearer token, so nothing but this list can stop
//! one being accepted. The rows carry the token's own expiry so the list stays
//! small: once a token has expired the signature check refuses it anyway, and
//! keeping the row would only grow the table for ever.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{Database, row::Timestamp};

/// Reads and writes `revoked_jtis`.
pub struct RevokedJtisRepo<'a> {
    db: &'a Database,
}

impl<'a> RevokedJtisRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Disowns a token. Revoking one twice is not an error.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke(&self, jti: &str, expires_at: DateTime<Utc>) -> Result<(), Error> {
        let jti = jti.to_owned();
        let expires_at = Timestamp::from(expires_at);

        self.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO revoked_jtis (jti, expires_at, revoked_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT (jti) DO NOTHING",
                    rusqlite::params![jti, expires_at, Timestamp::now()],
                )
            })
            .await?;

        Ok(())
    }

    /// Whether a token has been disowned.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn is_revoked(&self, jti: &str) -> Result<bool, Error> {
        let jti = jti.to_owned();

        let found: Option<i64> = self
            .db
            .read(move |c| {
                c.query_one("SELECT 1 FROM revoked_jtis WHERE jti = ?1", [jti], |row| {
                    row.get(0)
                })
                .optional()
            })
            .await?;

        Ok(found.is_some())
    }

    /// Every disowned token still worth checking, for the in-memory cache the
    /// request path reads.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn live(&self) -> Result<Vec<String>, Error> {
        self.db
            .read(|c| {
                let mut statement =
                    c.prepare("SELECT jti FROM revoked_jtis WHERE expires_at > ?1")?;

                statement
                    .query_map([Timestamp::now()], |row| row.get(0))?
                    .collect()
            })
            .await
    }

    /// Drops rows for tokens that have expired, which the signature check now
    /// refuses on its own.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn prune(&self) -> Result<usize, Error> {
        self.db
            .write(|tx| {
                tx.execute(
                    "DELETE FROM revoked_jtis WHERE expires_at <= ?1",
                    [Timestamp::now()],
                )
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
    async fn a_revoked_token_is_reported_as_revoked() {
        let db = db().await;

        db.revoked_jtis()
            .revoke("jti-1", Utc::now() + chrono::TimeDelta::hours(1))
            .await
            .unwrap();

        assert!(db.revoked_jtis().is_revoked("jti-1").await.unwrap());
        assert!(!db.revoked_jtis().is_revoked("jti-2").await.unwrap());
    }

    #[tokio::test]
    async fn revoking_the_same_token_twice_is_not_an_error() {
        let db = db().await;
        let expiry = Utc::now() + chrono::TimeDelta::hours(1);

        db.revoked_jtis().revoke("jti-1", expiry).await.unwrap();
        db.revoked_jtis().revoke("jti-1", expiry).await.unwrap();

        assert_eq!(
            db.revoked_jtis().live().await.unwrap(),
            vec!["jti-1".to_string()]
        );
    }

    #[tokio::test]
    async fn the_cache_only_carries_tokens_that_could_still_be_presented() {
        let db = db().await;
        db.revoked_jtis()
            .revoke("live", Utc::now() + chrono::TimeDelta::hours(1))
            .await
            .unwrap();
        db.revoked_jtis()
            .revoke("expired", Utc::now() - chrono::TimeDelta::hours(1))
            .await
            .unwrap();

        assert_eq!(
            db.revoked_jtis().live().await.unwrap(),
            vec!["live".to_string()]
        );
    }

    #[tokio::test]
    async fn pruning_drops_only_the_expired_rows() {
        let db = db().await;
        db.revoked_jtis()
            .revoke("live", Utc::now() + chrono::TimeDelta::hours(1))
            .await
            .unwrap();
        db.revoked_jtis()
            .revoke("expired", Utc::now() - chrono::TimeDelta::hours(1))
            .await
            .unwrap();

        assert_eq!(db.revoked_jtis().prune().await.unwrap(), 1);
        assert!(db.revoked_jtis().is_revoked("live").await.unwrap());
        assert!(!db.revoked_jtis().is_revoked("expired").await.unwrap());
    }
}
