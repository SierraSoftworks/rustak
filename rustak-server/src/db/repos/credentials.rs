//! `credentials`: the argon2id-hashed secrets a client can present.
//!
//! Three kinds, and no others: a one-time `enrollment_token`, an opt-in
//! expiring `client_password` for the clients that can only do Basic auth, and
//! a `service_token` for a sidecar's control API. There is no local password
//! kind, because there are no local passwords.
//!
//! # Finding the row without hashing everything
//!
//! A client offers a secret, not an id. `lookup_hint` is a cheap unsalted
//! sha256 prefix that narrows the candidates to (almost always) one row, which
//! is then verified properly with argon2. A hint match is a reason to *try* a
//! row and never on its own a reason to accept a secret — see
//! [`rustak_core::identity::password`].

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_api::CredentialKind;
use rustak_core::{identity::PasswordHash, prelude::*};

use crate::db::{
    Database,
    repos::Page,
    row::{Timestamp, enum_col, id_col, opt_ts, ts},
};

/// The columns [`CredentialRow::from_row`] expects, in order.
const COLUMNS: &str = "id, user_id, kind, label, secret_hash, lookup_hint, max_uses, uses, \
                       expires_at, last_used_at, revoked_at, created_by, created_at";

/// One row of `credentials`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRow {
    pub id: CredentialId,
    pub user_id: UserId,
    pub kind: CredentialKind,
    pub label: String,
    /// The argon2id PHC string. Redacts itself in `Debug` output.
    pub secret_hash: PasswordHash,
    pub lookup_hint: String,
    pub max_uses: Option<u32>,
    pub uses: u32,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_by: Option<Username>,
    pub created_at: DateTime<Utc>,
}

impl CredentialRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            user_id: id_col(row, 1)?,
            kind: enum_col(row, 2, CredentialKind::parse)?,
            label: row.get(3)?,
            secret_hash: PasswordHash::parse(row.get::<_, String>(4)?).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?,
            lookup_hint: row.get(5)?,
            max_uses: row
                .get::<_, Option<i64>>(6)?
                .map(|value| value.max(0) as u32),
            uses: row.get::<_, i64>(7)?.max(0) as u32,
            expires_at: opt_ts(row, 8)?,
            last_used_at: opt_ts(row, 9)?,
            revoked_at: opt_ts(row, 10)?,
            created_by: row
                .get::<_, Option<String>>(11)?
                .map(Username::from_storage),
            created_at: ts(row, 12)?,
        })
    }

    /// Whether this credential may be accepted at `now`.
    ///
    /// Revocation, expiry and exhaustion are all checked here rather than in
    /// SQL, so that a caller cannot accidentally use a query that forgot one.
    pub fn is_usable_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none()
            && self.expires_at.is_none_or(|expires| expires > now)
            && self.max_uses.is_none_or(|max| self.uses < max)
    }
}

/// A credential about to be minted.
#[derive(Debug, Clone)]
pub struct NewCredential {
    pub user_id: UserId,
    pub kind: CredentialKind,
    pub label: String,
    pub secret_hash: PasswordHash,
    pub lookup_hint: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub created_by: Option<Username>,
}

/// Reads and writes `credentials`.
pub struct CredentialsRepo<'a> {
    db: &'a Database,
}

impl<'a> CredentialsRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Stores a newly minted credential.
    ///
    /// The secret itself is never stored, and never reaches this method: what
    /// arrives is its hash and its lookup hint.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the user does not
    /// exist.
    pub async fn create(&self, new: NewCredential) -> Result<CredentialRow, Error> {
        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO credentials \
                           (user_id, kind, label, secret_hash, lookup_hint, max_uses, \
                            expires_at, created_by, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.user_id.get(),
                        new.kind.as_str(),
                        new.label,
                        new.secret_hash.as_str(),
                        new.lookup_hint,
                        new.max_uses.map(i64::from),
                        new.expires_at.map(Timestamp::from),
                        new.created_by.map(|by| by.into_inner()),
                        Timestamp::now(),
                    ],
                    CredentialRow::from_row,
                )
            })
            .await
    }

    /// Reads one credential by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: CredentialId) -> Result<Option<CredentialRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM credentials WHERE id = ?1"),
                    [id.get()],
                    CredentialRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// The candidate rows for a secret whose hint is `hint`.
    ///
    /// Returns every match, not one: the hint is short enough to collide, and
    /// picking the first would make a collision an authentication failure
    /// instead of an extra argon2 verification.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn find_by_hint(&self, hint: &str) -> Result<Vec<CredentialRow>, Error> {
        let hint = hint.to_owned();

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM credentials \
                     WHERE lookup_hint = ?1 AND revoked_at IS NULL ORDER BY id ASC"
                ))?;

                statement
                    .query_map([hint], CredentialRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Every credential a user holds, newest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_for_user(
        &self,
        user_id: UserId,
        include_revoked: bool,
    ) -> Result<Vec<CredentialRow>, Error> {
        self.db
            .read(move |c| {
                let filter = if include_revoked {
                    ""
                } else {
                    "AND revoked_at IS NULL"
                };
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM credentials \
                     WHERE user_id = ?1 {filter} ORDER BY id DESC"
                ))?;

                statement
                    .query_map([user_id.get()], CredentialRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Every credential this installation holds, newest first.
    ///
    /// Paged, because unlike an account's own list this one has no natural
    /// bound: it is what an operator auditing outstanding client passwords
    /// reads, and an installation that has been enrolling devices for a year
    /// has a row per device per re-enrolment.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_all(
        &self,
        include_revoked: bool,
        page: Page,
    ) -> Result<Vec<CredentialRow>, Error> {
        self.db
            .read(move |c| {
                let filter = if include_revoked {
                    ""
                } else {
                    "WHERE revoked_at IS NULL"
                };
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM credentials {filter} \
                     ORDER BY id DESC LIMIT ?1 OFFSET ?2"
                ))?;

                statement
                    .query_map(
                        rusqlite::params![page.limit(), page.offset()],
                        CredentialRow::from_row,
                    )?
                    .collect()
            })
            .await
    }

    /// Records a successful use, and spends the credential when `consumed`.
    ///
    /// `consumed` is set only where a use is meant to be terminal — an
    /// enrolment token that has just produced a certificate — so that a client
    /// password's use count rises without ever exhausting it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn record_use(&self, id: CredentialId, consumed: bool) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                let now = Timestamp::now();
                tx.execute(
                    "UPDATE credentials SET uses = uses + 1, last_used_at = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), now],
                )?;

                if consumed {
                    tx.execute(
                        "UPDATE credentials SET revoked_at = ?2 \
                         WHERE id = ?1 AND revoked_at IS NULL",
                        rusqlite::params![id.get(), now],
                    )?;
                }

                Ok(())
            })
            .await
    }

    /// Revokes a credential, reporting whether it was live.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke(&self, id: CredentialId) -> Result<bool, Error> {
        let revoked = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE credentials SET revoked_at = ?2 \
                     WHERE id = ?1 AND revoked_at IS NULL",
                    rusqlite::params![id.get(), Timestamp::now()],
                )
            })
            .await?;

        Ok(revoked > 0)
    }

    /// Revokes every live credential a user holds, for a disabled account.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke_all_for_user(&self, user_id: UserId) -> Result<usize, Error> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE credentials SET revoked_at = ?2 \
                     WHERE user_id = ?1 AND revoked_at IS NULL",
                    rusqlite::params![user_id.get(), Timestamp::now()],
                )
            })
            .await
    }

    /// Deletes credentials that expired or were revoked before `before`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn prune(&self, before: DateTime<Utc>) -> Result<usize, Error> {
        let before = Timestamp::from(before);

        self.db
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM credentials \
                     WHERE (revoked_at IS NOT NULL AND revoked_at < ?1) \
                        OR (expires_at IS NOT NULL AND expires_at < ?1)",
                    [before],
                )
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

    fn minted(user_id: UserId, kind: CredentialKind, secret: &str) -> NewCredential {
        NewCredential {
            user_id,
            kind,
            label: "Phone".into(),
            secret_hash: rustak_core::identity::hash(secret).unwrap(),
            lookup_hint: rustak_core::identity::lookup_hint(secret),
            expires_at: None,
            max_uses: None,
            created_by: Some(Username::parse("admin").unwrap()),
        }
    }

    #[tokio::test]
    async fn a_credential_reads_back_as_minted() {
        let (db, user) = fixture().await;

        let created = db
            .credentials()
            .create(NewCredential {
                max_uses: Some(1),
                ..minted(user, CredentialKind::EnrollmentToken, "s3cret")
            })
            .await
            .unwrap();

        assert_eq!(created.kind, CredentialKind::EnrollmentToken);
        assert_eq!(created.uses, 0);
        assert_eq!(created.max_uses, Some(1));
        assert_eq!(created.created_by.as_ref().unwrap().as_str(), "admin");
        assert!(created.is_usable_at(Utc::now()));
        assert_eq!(
            db.credentials().get(created.id).await.unwrap().unwrap(),
            created
        );
    }

    #[tokio::test]
    async fn the_secret_hash_redacts_itself_in_debug_output() {
        let (db, user) = fixture().await;
        let created = db
            .credentials()
            .create(minted(user, CredentialKind::ClientPassword, "s3cret"))
            .await
            .unwrap();

        let rendered = format!("{created:?}");

        assert!(!rendered.contains("$argon2id$"), "{rendered}");
        assert!(rendered.contains("PasswordHash(***)"));
    }

    #[tokio::test]
    async fn only_the_three_kinds_the_plan_keeps_are_storable() {
        let (db, user) = fixture().await;

        for kind in ["device_password", "local_password", "password"] {
            let refused = db
                .write(move |tx| {
                    tx.execute(
                        "INSERT INTO credentials \
                           (user_id, kind, label, secret_hash, lookup_hint, created_at) \
                         VALUES (?1, ?2, 'x', 'x', 'x', '2026-01-01T00:00:00.000Z')",
                        rusqlite::params![user.get(), kind],
                    )
                })
                .await;

            assert!(refused.is_err(), "{kind} should not be storable");
        }
    }

    #[tokio::test]
    async fn a_secret_is_found_by_its_hint_and_collisions_all_come_back() {
        let (db, user) = fixture().await;
        let hint = rustak_core::identity::lookup_hint("s3cret");

        db.credentials()
            .create(minted(user, CredentialKind::EnrollmentToken, "s3cret"))
            .await
            .unwrap();
        db.credentials()
            .create(NewCredential {
                // A different secret that happens to share a hint: contrived
                // here, but the point is that both rows must be offered.
                lookup_hint: hint.clone(),
                ..minted(user, CredentialKind::ClientPassword, "another")
            })
            .await
            .unwrap();

        assert_eq!(db.credentials().find_by_hint(&hint).await.unwrap().len(), 2);
        assert!(
            db.credentials()
                .find_by_hint("00000000")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn spending_a_one_time_token_makes_it_unusable() {
        let (db, user) = fixture().await;
        let token = db
            .credentials()
            .create(NewCredential {
                max_uses: Some(1),
                ..minted(user, CredentialKind::EnrollmentToken, "s3cret")
            })
            .await
            .unwrap();

        db.credentials().record_use(token.id, true).await.unwrap();

        let read = db.credentials().get(token.id).await.unwrap().unwrap();
        assert_eq!(read.uses, 1);
        assert!(read.revoked_at.is_some());
        assert!(!read.is_usable_at(Utc::now()));
        assert!(
            db.credentials()
                .find_by_hint(&read.lookup_hint)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_client_password_survives_being_used() {
        let (db, user) = fixture().await;
        let password = db
            .credentials()
            .create(minted(user, CredentialKind::ClientPassword, "s3cret"))
            .await
            .unwrap();

        db.credentials()
            .record_use(password.id, false)
            .await
            .unwrap();

        let read = db.credentials().get(password.id).await.unwrap().unwrap();
        assert_eq!(read.uses, 1);
        assert!(read.is_usable_at(Utc::now()));
    }

    #[tokio::test]
    async fn an_expired_credential_is_not_usable() {
        let (db, user) = fixture().await;
        let expired = db
            .credentials()
            .create(NewCredential {
                expires_at: Some(Utc::now() - chrono::TimeDelta::hours(1)),
                ..minted(user, CredentialKind::EnrollmentToken, "s3cret")
            })
            .await
            .unwrap();

        assert!(!expired.is_usable_at(Utc::now()));
        // Still returned by the hint lookup, because refusing it is the
        // verifier's job and it has to be able to say *why*.
        assert_eq!(
            db.credentials()
                .find_by_hint(&expired.lookup_hint)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn revoking_is_idempotent_and_listing_can_hide_the_revoked() {
        let (db, user) = fixture().await;
        let credential = db
            .credentials()
            .create(minted(user, CredentialKind::ClientPassword, "s3cret"))
            .await
            .unwrap();

        assert!(db.credentials().revoke(credential.id).await.unwrap());
        assert!(!db.credentials().revoke(credential.id).await.unwrap());

        assert!(
            db.credentials()
                .list_for_user(user, false)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.credentials()
                .list_for_user(user, true)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn disabling_an_account_can_revoke_everything_it_holds() {
        let (db, user) = fixture().await;
        db.credentials()
            .create(minted(user, CredentialKind::ClientPassword, "a"))
            .await
            .unwrap();
        db.credentials()
            .create(minted(user, CredentialKind::ServiceToken, "b"))
            .await
            .unwrap();

        assert_eq!(db.credentials().revoke_all_for_user(user).await.unwrap(), 2);
        assert!(
            db.credentials()
                .list_for_user(user, false)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pruning_removes_what_is_finished_with_and_keeps_what_is_live() {
        let (db, user) = fixture().await;
        db.credentials()
            .create(NewCredential {
                expires_at: Some(Utc::now() - chrono::TimeDelta::days(60)),
                ..minted(user, CredentialKind::EnrollmentToken, "old")
            })
            .await
            .unwrap();
        db.credentials()
            .create(minted(user, CredentialKind::ClientPassword, "live"))
            .await
            .unwrap();

        assert_eq!(
            db.credentials()
                .prune(Utc::now() - chrono::TimeDelta::days(30))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.credentials()
                .list_for_user(user, true)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn deleting_a_user_takes_their_credentials_with_them() {
        let (db, user) = fixture().await;
        let credential = db
            .credentials()
            .create(minted(user, CredentialKind::ClientPassword, "s3cret"))
            .await
            .unwrap();

        db.users().delete(user).await.unwrap();

        assert!(db.credentials().get(credential.id).await.unwrap().is_none());
    }
}
