//! `passkeys`: WebAuthn credentials, which are how a local administrator signs
//! in to the admin UI.
//!
//! These live apart from `credentials` because what is kept is a *public* key
//! rather than the hash of a secret: nothing here is verified with argon2, and
//! nothing here is worth stealing. The sign counter is the one mutable part,
//! and it exists to notice a cloned authenticator.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    row::{Timestamp, bool_col, id_col, opt_json_col, opt_ts, to_json, ts},
};

/// The columns [`PasskeyRow::from_row`] expects, in order.
const COLUMNS: &str = "id, user_id, credential_id, public_key, sign_count, transports, label, \
                       backup_eligible, backup_state, created_at, last_used_at";

/// One row of `passkeys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyRow {
    pub id: PasskeyId,
    pub user_id: UserId,
    /// The authenticator's own credential id, as raw bytes.
    pub credential_id: Vec<u8>,
    /// The COSE public key, as the authenticator gave it.
    pub public_key: Vec<u8>,
    /// The authenticator's signature counter at its last use. A counter that
    /// goes backwards is the signal that a credential has been cloned.
    pub sign_count: u32,
    /// `["usb", "nfc", …]`, which the browser uses to suggest how to sign in.
    pub transports: Option<Vec<String>>,
    pub label: String,
    /// Whether the credential may be backed up to a passkey provider.
    pub backup_eligible: bool,
    /// Whether it currently is.
    pub backup_state: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl PasskeyRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            user_id: id_col(row, 1)?,
            credential_id: row.get(2)?,
            public_key: row.get(3)?,
            sign_count: row.get::<_, i64>(4)?.max(0) as u32,
            transports: opt_json_col(row, 5)?,
            label: row.get(6)?,
            backup_eligible: bool_col(row, 7)?,
            backup_state: bool_col(row, 8)?,
            created_at: ts(row, 9)?,
            last_used_at: opt_ts(row, 10)?,
        })
    }
}

/// A passkey about to be registered.
#[derive(Debug, Clone)]
pub struct NewPasskey {
    pub user_id: UserId,
    pub credential_id: Vec<u8>,
    pub public_key: Vec<u8>,
    pub sign_count: u32,
    pub transports: Option<Vec<String>>,
    pub label: String,
    pub backup_eligible: bool,
    pub backup_state: bool,
}

/// Reads and writes `passkeys`.
pub struct PasskeysRepo<'a> {
    db: &'a Database,
}

impl<'a> PasskeysRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Registers a passkey.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the credential id
    /// is already registered — to anybody.
    pub async fn create(&self, new: NewPasskey) -> Result<PasskeyRow, Error> {
        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO passkeys \
                           (user_id, credential_id, public_key, sign_count, transports, label, \
                            backup_eligible, backup_state, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.user_id.get(),
                        new.credential_id,
                        new.public_key,
                        i64::from(new.sign_count),
                        new.transports.map(|t| to_json(&t)).transpose()?,
                        new.label,
                        i64::from(new.backup_eligible),
                        i64::from(new.backup_state),
                        Timestamp::now(),
                    ],
                    PasskeyRow::from_row,
                )
            })
            .await
    }

    /// Reads one passkey by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: PasskeyId) -> Result<Option<PasskeyRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM passkeys WHERE id = ?1"),
                    [id.get()],
                    PasskeyRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads the passkey an authenticator just identified itself with.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_credential_id(
        &self,
        credential_id: &[u8],
    ) -> Result<Option<PasskeyRow>, Error> {
        let credential_id = credential_id.to_vec();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM passkeys WHERE credential_id = ?1"),
                    [credential_id],
                    PasskeyRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Every passkey a user has registered, newest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_for_user(&self, user_id: UserId) -> Result<Vec<PasskeyRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM passkeys WHERE user_id = ?1 ORDER BY id DESC"
                ))?;

                statement
                    .query_map([user_id.get()], PasskeyRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Records a sign-in, refusing a counter that has gone backwards.
    ///
    /// An authenticator's counter only ever increases. One that repeats or
    /// regresses means the credential exists in two places, which is the whole
    /// reason the counter is there — so the sign-in is refused rather than
    /// quietly accepted. A counter of zero means the authenticator does not
    /// keep one, and is exempt.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the counter regressed, and a
    /// [`human_errors::Kind::System`] error if the write fails.
    pub async fn record_use(&self, id: PasskeyId, sign_count: u32) -> Result<(), Error> {
        let accepted = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE passkeys SET sign_count = ?2, last_used_at = ?3 \
                     WHERE id = ?1 AND (?2 > sign_count OR ?2 = 0)",
                    rusqlite::params![id.get(), i64::from(sign_count), Timestamp::now()],
                )
            })
            .await?;

        if accepted == 0 {
            return Err(human_errors::user(
                "That security key's counter did not move, which can mean it has been cloned.",
                &[
                    "Sign in with another passkey and remove this one.",
                    "If you have not shared this key, contact your administrator.",
                ],
            ));
        }

        Ok(())
    }

    /// Renames a passkey.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn rename(&self, id: PasskeyId, label: String) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE passkeys SET label = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), label],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Removes a passkey.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, id: PasskeyId) -> Result<bool, Error> {
        let deleted = self
            .db
            .write(move |tx| tx.execute("DELETE FROM passkeys WHERE id = ?1", [id.get()]))
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
            .create(NewUser::person(Username::parse("admin").unwrap()))
            .await
            .unwrap();

        (db, user.id)
    }

    fn registered(user_id: UserId, credential_id: &[u8]) -> NewPasskey {
        NewPasskey {
            user_id,
            credential_id: credential_id.to_vec(),
            public_key: b"cose-public-key".to_vec(),
            sign_count: 1,
            transports: Some(vec!["internal".into(), "hybrid".into()]),
            label: "Laptop".into(),
            backup_eligible: true,
            backup_state: false,
        }
    }

    #[tokio::test]
    async fn a_passkey_reads_back_as_registered() {
        let (db, user) = fixture().await;

        let created = db
            .passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();

        assert_eq!(created.credential_id, b"cred-1".to_vec());
        assert_eq!(created.public_key, b"cose-public-key".to_vec());
        assert_eq!(
            created.transports,
            Some(vec!["internal".to_string(), "hybrid".to_string()])
        );
        assert!(created.backup_eligible);
        assert!(!created.backup_state);
        assert_eq!(
            db.passkeys().get(created.id).await.unwrap().unwrap(),
            created
        );
    }

    #[tokio::test]
    async fn a_credential_id_can_only_be_registered_once() {
        let (db, user) = fixture().await;
        db.passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();

        assert!(
            db.passkeys()
                .create(registered(user, b"cred-1"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_authenticator_is_found_by_the_id_it_presents() {
        let (db, user) = fixture().await;
        let created = db
            .passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();

        assert_eq!(
            db.passkeys()
                .get_by_credential_id(b"cred-1")
                .await
                .unwrap()
                .unwrap()
                .id,
            created.id
        );
        assert!(
            db.passkeys()
                .get_by_credential_id(b"other")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_counter_that_moves_forward_is_recorded() {
        let (db, user) = fixture().await;
        let passkey = db
            .passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();

        db.passkeys().record_use(passkey.id, 2).await.unwrap();

        let read = db.passkeys().get(passkey.id).await.unwrap().unwrap();
        assert_eq!(read.sign_count, 2);
        assert!(read.last_used_at.is_some());
    }

    #[tokio::test]
    async fn a_counter_that_repeats_or_regresses_is_refused() {
        let (db, user) = fixture().await;
        let passkey = db
            .passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();
        db.passkeys().record_use(passkey.id, 5).await.unwrap();

        assert!(db.passkeys().record_use(passkey.id, 5).await.is_err());
        assert!(db.passkeys().record_use(passkey.id, 4).await.is_err());
        assert_eq!(
            db.passkeys()
                .get(passkey.id)
                .await
                .unwrap()
                .unwrap()
                .sign_count,
            5
        );
    }

    #[tokio::test]
    async fn an_authenticator_that_keeps_no_counter_is_exempt() {
        let (db, user) = fixture().await;
        let passkey = db
            .passkeys()
            .create(NewPasskey {
                sign_count: 0,
                ..registered(user, b"cred-1")
            })
            .await
            .unwrap();

        db.passkeys().record_use(passkey.id, 0).await.unwrap();
        db.passkeys().record_use(passkey.id, 0).await.unwrap();

        assert_eq!(
            db.passkeys()
                .get(passkey.id)
                .await
                .unwrap()
                .unwrap()
                .sign_count,
            0
        );
    }

    #[tokio::test]
    async fn passkeys_can_be_renamed_listed_and_removed() {
        let (db, user) = fixture().await;
        let first = db
            .passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();
        db.passkeys()
            .create(registered(user, b"cred-2"))
            .await
            .unwrap();

        assert!(
            db.passkeys()
                .rename(first.id, "Phone".into())
                .await
                .unwrap()
        );
        assert_eq!(db.passkeys().list_for_user(user).await.unwrap().len(), 2);
        assert!(db.passkeys().delete(first.id).await.unwrap());
        assert!(!db.passkeys().delete(first.id).await.unwrap());
        assert_eq!(db.passkeys().list_for_user(user).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deleting_a_user_takes_their_passkeys_with_them() {
        let (db, user) = fixture().await;
        db.passkeys()
            .create(registered(user, b"cred-1"))
            .await
            .unwrap();

        db.users().delete(user).await.unwrap();

        assert!(
            db.passkeys()
                .get_by_credential_id(b"cred-1")
                .await
                .unwrap()
                .is_none()
        );
    }
}
