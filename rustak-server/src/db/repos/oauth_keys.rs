//! `oauth_keys`: the keys this server signs tokens with.
//!
//! Two purposes, and they must not share a key. `access_token` keys are RS256
//! and their public half is published at the JWKS endpoint so anything can
//! verify a token we issued. `mission_token` keys are HS256 with no public half
//! at all, because a mission token is only ever verified by us — publishing a
//! key that is also the verifier would let anybody mint one.
//!
//! Retiring a key rather than deleting it is what makes rotation safe: tokens
//! signed by the old key stay verifiable until they expire.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    row::{Timestamp, enum_col, opt_ts, ts},
};

/// The columns [`OauthKeyRow::from_row`] expects, in order.
const COLUMNS: &str = "kid, alg, purpose, public_jwk, private_sealed, created_at, retired_at";

/// The signature algorithm a key is used with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgorithm {
    /// RSA with SHA-256, for the tokens other software verifies.
    Rs256,
    /// HMAC with SHA-256, for the tokens only we verify.
    Hs256,
}

impl KeyAlgorithm {
    /// The value stored in the column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Hs256 => "HS256",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "RS256" => Some(Self::Rs256),
            "HS256" => Some(Self::Hs256),
            _ => None,
        }
    }
}

/// What a key signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPurpose {
    /// Our own bearer tokens, for the admin UI, CloudTAK and sidecars.
    AccessToken,
    /// The HS256 mission tokens the Marti API hands out.
    MissionToken,
}

impl KeyPurpose {
    /// The value stored in the column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AccessToken => "access_token",
            Self::MissionToken => "mission_token",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "access_token" => Some(Self::AccessToken),
            "mission_token" => Some(Self::MissionToken),
            _ => None,
        }
    }
}

/// One row of `oauth_keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthKeyRow {
    /// The `kid` a token's header carries.
    pub kid: String,
    pub alg: KeyAlgorithm,
    pub purpose: KeyPurpose,
    /// The public half as a JWK, for the JWKS endpoint. `None` for HS256.
    pub public_jwk: Option<String>,
    /// The private half, sealed.
    pub private_sealed: String,
    pub created_at: DateTime<Utc>,
    /// Set when the key stopped being used for signing. Tokens it signed stay
    /// verifiable until they expire.
    pub retired_at: Option<DateTime<Utc>>,
}

impl OauthKeyRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            kid: row.get(0)?,
            alg: enum_col(row, 1, KeyAlgorithm::parse)?,
            purpose: enum_col(row, 2, KeyPurpose::parse)?,
            public_jwk: row.get(3)?,
            private_sealed: row.get(4)?,
            created_at: ts(row, 5)?,
            retired_at: opt_ts(row, 6)?,
        })
    }
}

/// A key about to be stored.
#[derive(Debug, Clone)]
pub struct NewOauthKey {
    pub kid: String,
    pub alg: KeyAlgorithm,
    pub purpose: KeyPurpose,
    pub public_jwk: Option<String>,
    pub private_sealed: String,
}

/// Reads and writes `oauth_keys`.
pub struct OauthKeysRepo<'a> {
    db: &'a Database,
}

impl<'a> OauthKeysRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Stores a key.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the key id is
    /// already in use.
    pub async fn create(&self, new: NewOauthKey) -> Result<OauthKeyRow, Error> {
        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO oauth_keys \
                           (kid, alg, purpose, public_jwk, private_sealed, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.kid,
                        new.alg.as_str(),
                        new.purpose.as_str(),
                        new.public_jwk,
                        new.private_sealed,
                        Timestamp::now(),
                    ],
                    OauthKeyRow::from_row,
                )
            })
            .await
    }

    /// Reads one key by its id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, kid: &str) -> Result<Option<OauthKeyRow>, Error> {
        let kid = kid.to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM oauth_keys WHERE kid = ?1"),
                    [kid],
                    OauthKeyRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// The newest live key for a purpose: the one to sign with.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn active(&self, purpose: KeyPurpose) -> Result<Option<OauthKeyRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!(
                        "SELECT {COLUMNS} FROM oauth_keys \
                         WHERE purpose = ?1 AND retired_at IS NULL \
                         ORDER BY created_at DESC, kid DESC LIMIT 1"
                    ),
                    [purpose.as_str()],
                    OauthKeyRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Every key for a purpose, live and retired: what verification needs.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, purpose: KeyPurpose) -> Result<Vec<OauthKeyRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM oauth_keys WHERE purpose = ?1 \
                     ORDER BY created_at DESC, kid DESC"
                ))?;

                statement
                    .query_map([purpose.as_str()], OauthKeyRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Stops signing with a key, without making the tokens it signed
    /// unverifiable.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn retire(&self, kid: &str) -> Result<bool, Error> {
        let kid = kid.to_owned();

        let retired = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE oauth_keys SET retired_at = ?2 \
                     WHERE kid = ?1 AND retired_at IS NULL",
                    rusqlite::params![kid, Timestamp::now()],
                )
            })
            .await?;

        Ok(retired > 0)
    }

    /// Deletes keys retired before `before`, once nothing they signed can still
    /// be presented.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn prune(&self, before: DateTime<Utc>) -> Result<usize, Error> {
        let before = Timestamp::from(before);

        self.db
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM oauth_keys WHERE retired_at IS NOT NULL AND retired_at < ?1",
                    [before],
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

    fn access_key(kid: &str) -> NewOauthKey {
        NewOauthKey {
            kid: kid.into(),
            alg: KeyAlgorithm::Rs256,
            purpose: KeyPurpose::AccessToken,
            public_jwk: Some(r#"{"kty":"RSA","n":"…","e":"AQAB"}"#.into()),
            private_sealed: r#"{"nonce":"…","ciphertext":"…"}"#.into(),
        }
    }

    fn mission_key(kid: &str) -> NewOauthKey {
        NewOauthKey {
            kid: kid.into(),
            alg: KeyAlgorithm::Hs256,
            purpose: KeyPurpose::MissionToken,
            public_jwk: None,
            private_sealed: r#"{"nonce":"…","ciphertext":"…"}"#.into(),
        }
    }

    #[tokio::test]
    async fn a_key_reads_back_as_stored() {
        let db = db().await;

        let created = db.oauth_keys().create(access_key("k1")).await.unwrap();

        assert_eq!(created.alg, KeyAlgorithm::Rs256);
        assert_eq!(created.purpose, KeyPurpose::AccessToken);
        assert!(created.retired_at.is_none());
        assert_eq!(db.oauth_keys().get("k1").await.unwrap().unwrap(), created);
    }

    #[tokio::test]
    async fn a_mission_key_has_no_public_half() {
        let db = db().await;

        let created = db.oauth_keys().create(mission_key("m1")).await.unwrap();

        assert_eq!(created.public_jwk, None);
        assert_eq!(created.alg, KeyAlgorithm::Hs256);
    }

    #[tokio::test]
    async fn a_key_id_is_used_once() {
        let db = db().await;
        db.oauth_keys().create(access_key("k1")).await.unwrap();

        assert!(db.oauth_keys().create(access_key("k1")).await.is_err());
    }

    #[tokio::test]
    async fn an_unknown_algorithm_or_purpose_is_refused_by_the_schema() {
        let db = db().await;

        for (alg, purpose) in [("ES256", "access_token"), ("RS256", "everything")] {
            let refused = db
                .write(move |tx| {
                    tx.execute(
                        "INSERT INTO oauth_keys (kid, alg, purpose, private_sealed, created_at) \
                         VALUES ('x', ?1, ?2, '{}', '2026-01-01T00:00:00.000Z')",
                        [alg, purpose],
                    )
                })
                .await;

            assert!(refused.is_err(), "{alg}/{purpose} should be refused");
        }
    }

    #[tokio::test]
    async fn the_active_key_is_the_newest_live_one_for_that_purpose() {
        let db = db().await;
        let first = db.oauth_keys().create(access_key("k1")).await.unwrap();
        db.oauth_keys().create(mission_key("m1")).await.unwrap();

        assert_eq!(
            db.oauth_keys()
                .active(KeyPurpose::AccessToken)
                .await
                .unwrap()
                .unwrap()
                .kid,
            first.kid
        );

        let second = db.oauth_keys().create(access_key("k2")).await.unwrap();
        db.oauth_keys().retire(&first.kid).await.unwrap();

        assert_eq!(
            db.oauth_keys()
                .active(KeyPurpose::AccessToken)
                .await
                .unwrap()
                .unwrap()
                .kid,
            second.kid
        );
    }

    #[tokio::test]
    async fn a_retired_key_can_still_be_read_for_verification() {
        let db = db().await;
        db.oauth_keys().create(access_key("k1")).await.unwrap();

        assert!(db.oauth_keys().retire("k1").await.unwrap());
        assert!(!db.oauth_keys().retire("k1").await.unwrap());

        assert!(
            db.oauth_keys()
                .get("k1")
                .await
                .unwrap()
                .unwrap()
                .retired_at
                .is_some()
        );
        assert_eq!(
            db.oauth_keys()
                .list(KeyPurpose::AccessToken)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.oauth_keys()
                .active(KeyPurpose::AccessToken)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn pruning_removes_only_keys_retired_long_enough_ago() {
        let db = db().await;
        db.oauth_keys().create(access_key("k1")).await.unwrap();
        db.oauth_keys().create(access_key("k2")).await.unwrap();
        db.oauth_keys().retire("k1").await.unwrap();

        assert_eq!(
            db.oauth_keys()
                .prune(Utc::now() - chrono::TimeDelta::days(30))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            db.oauth_keys()
                .prune(Utc::now() + chrono::TimeDelta::seconds(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.oauth_keys()
                .list(KeyPurpose::AccessToken)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
