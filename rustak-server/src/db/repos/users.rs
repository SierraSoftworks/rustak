//! `users`: the people and services who may connect.
//!
//! There is no password column. Local sign-in is by passkey and every other
//! credential is a row in `credentials`, so there is nowhere here for a password
//! to be stored even by accident.
//!
//! The display name and email an administrator types are written by [`profile`],
//! a child module, because this file is at `conventions.md`'s length limit.

pub mod profile;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_api::{UserKind, UserSource};
use rustak_core::prelude::*;

use crate::db::{
    Database,
    repos::Page,
    row::{bool_col, enum_col, id_col, opt_ts, ts},
};

/// The columns [`UserRow::from_row`] expects, in order.
const COLUMNS: &str = "id, username, kind, display_name, email, is_admin, admin_override, \
                       disabled, source, oidc_issuer, oidc_subject, created_at, updated_at, \
                       last_seen_at, last_login_at";

/// One row of `users`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    pub id: UserId,
    pub username: Username,
    pub kind: UserKind,
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// What the last sign-in decided, unless `admin_override` says otherwise.
    pub is_admin: bool,
    /// An administrator's explicit decision, which outranks the ACL.
    pub admin_override: Option<bool>,
    pub disabled: bool,
    pub source: UserSource,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl UserRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            username: Username::from_storage(row.get::<_, String>(1)?),
            kind: enum_col(row, 2, UserKind::parse)?,
            display_name: row.get(3)?,
            email: row.get(4)?,
            is_admin: bool_col(row, 5)?,
            admin_override: row.get::<_, Option<i64>>(6)?.map(|value| value != 0),
            disabled: bool_col(row, 7)?,
            source: enum_col(row, 8, UserSource::parse)?,
            oidc_issuer: row.get(9)?,
            oidc_subject: row.get(10)?,
            created_at: ts(row, 11)?,
            updated_at: ts(row, 12)?,
            last_seen_at: opt_ts(row, 13)?,
            last_login_at: opt_ts(row, 14)?,
        })
    }

    /// Whether this account may act as an administrator right now.
    pub fn is_effective_admin(&self) -> bool {
        self.admin_override.unwrap_or(self.is_admin) && !self.disabled
    }
}

/// A user about to be created.
#[derive(Debug, Clone)]
pub struct NewUser {
    pub username: Username,
    pub kind: UserKind,
    pub source: UserSource,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_admin: bool,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
}

impl NewUser {
    /// A person signing in for the first time, or added by an administrator.
    pub fn person(username: Username) -> Self {
        Self {
            username,
            kind: UserKind::Person,
            source: UserSource::Local,
            display_name: None,
            email: None,
            is_admin: false,
            oidc_issuer: None,
            oidc_subject: None,
        }
    }

    /// A sidecar's own account.
    pub fn service(username: Username) -> Self {
        Self {
            kind: UserKind::Service,
            source: UserSource::Service,
            ..Self::person(username)
        }
    }
}

/// What an identity provider told us about somebody.
#[derive(Debug, Clone, Default)]
pub struct OidcProfile {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_admin: bool,
}

/// Reads and writes `users`.
pub struct UsersRepo<'a> {
    db: &'a Database,
}

impl<'a> UsersRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Creates a user.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the username is
    /// already taken — usernames are unique without regard to case.
    pub async fn create(&self, new: NewUser) -> Result<UserRow, Error> {
        self.db
            .write(move |tx| {
                let now = crate::db::row::Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO users \
                           (username, kind, display_name, email, is_admin, source, \
                            oidc_issuer, oidc_subject, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.username.as_str(),
                        new.kind.as_str(),
                        new.display_name,
                        new.email,
                        i64::from(new.is_admin),
                        new.source.as_str(),
                        new.oidc_issuer,
                        new.oidc_subject,
                        now,
                    ],
                    UserRow::from_row,
                )
            })
            .await
    }

    /// Reads one user by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: UserId) -> Result<Option<UserRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM users WHERE id = ?1"),
                    [id.get()],
                    UserRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads one user by name, without regard to case.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_username(&self, username: &Username) -> Result<Option<UserRow>, Error> {
        let username = username.as_str().to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM users WHERE username = ?1"),
                    [username],
                    UserRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads one user by the identity-provider subject they signed in with.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_oidc(&self, issuer: &str, subject: &str) -> Result<Option<UserRow>, Error> {
        let (issuer, subject) = (issuer.to_owned(), subject.to_owned());

        self.db
            .read(move |c| {
                c.query_one(
                    &format!(
                        "SELECT {COLUMNS} FROM users WHERE oidc_issuer = ?1 AND oidc_subject = ?2"
                    ),
                    [issuer, subject],
                    UserRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Creates or refreshes the account behind an identity-provider subject.
    ///
    /// The subject is the identity, not the username: a provider that lets
    /// somebody rename themselves must not strand their account or let them
    /// walk into another one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn upsert_oidc(
        &self,
        issuer: &str,
        subject: &str,
        username: &Username,
        profile: OidcProfile,
    ) -> Result<UserRow, Error> {
        let (issuer, subject) = (issuer.to_owned(), subject.to_owned());
        let username = username.as_str().to_owned();

        self.db
            .write(move |tx| {
                let now = crate::db::row::Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO users \
                           (username, kind, display_name, email, is_admin, source, \
                            oidc_issuer, oidc_subject, created_at, updated_at, last_login_at) \
                         VALUES (?1, 'person', ?2, ?3, ?4, 'oidc', ?5, ?6, ?7, ?7, ?7) \
                         ON CONFLICT (oidc_issuer, oidc_subject) \
                           WHERE oidc_subject IS NOT NULL DO UPDATE SET \
                           username = excluded.username, \
                           display_name = excluded.display_name, \
                           email = excluded.email, \
                           is_admin = excluded.is_admin, \
                           source = 'oidc', \
                           updated_at = excluded.updated_at, \
                           last_login_at = excluded.last_login_at \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        username,
                        profile.display_name,
                        profile.email,
                        i64::from(profile.is_admin),
                        issuer,
                        subject,
                        now,
                    ],
                    UserRow::from_row,
                )
            })
            .await
    }

    /// Lists users by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, page: Page) -> Result<Vec<UserRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM users ORDER BY username ASC LIMIT ?1 OFFSET ?2"
                ))?;

                statement
                    .query_map([page.limit(), page.offset()], UserRow::from_row)?
                    .collect()
            })
            .await
    }

    /// How many accounts can currently administer the server.
    ///
    /// The setup wizard and the "disable this account" path both ask, so that
    /// an installation cannot be left with nobody who can get back into it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn count_admins(&self) -> Result<u64, Error> {
        let count: i64 = self
            .db
            .read(|c| {
                c.query_one(
                    "SELECT COUNT(*) FROM users \
                     WHERE disabled = 0 AND COALESCE(admin_override, is_admin) = 1",
                    [],
                    |row| row.get(0),
                )
            })
            .await?;

        Ok(count.max(0) as u64)
    }

    /// Disables or re-enables an account, reporting whether a row changed.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_disabled(&self, id: UserId, disabled: bool) -> Result<bool, Error> {
        self.update_flag(id, "disabled = ?2", i64::from(disabled))
            .await
    }

    /// Pins admin on or off regardless of what the ACL decides at sign-in.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_admin_override(&self, id: UserId, admin: Option<bool>) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE users SET admin_override = ?2, updated_at = ?3 WHERE id = ?1",
                    rusqlite::params![
                        id.get(),
                        admin.map(i64::from),
                        crate::db::row::Timestamp::now()
                    ],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Records that we have just seen this user on a listener.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn touch_last_seen(&self, id: UserId) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE users SET last_seen_at = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), crate::db::row::Timestamp::now()],
                )
            })
            .await?;

        Ok(())
    }

    /// Deletes a user, and with them their devices, credentials and
    /// memberships.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, id: UserId) -> Result<bool, Error> {
        let deleted = self
            .db
            .write(move |tx| tx.execute("DELETE FROM users WHERE id = ?1", [id.get()]))
            .await?;

        Ok(deleted > 0)
    }

    async fn update_flag(
        &self,
        id: UserId,
        assignment: &'static str,
        value: i64,
    ) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    &format!("UPDATE users SET {assignment}, updated_at = ?3 WHERE id = ?1"),
                    rusqlite::params![id.get(), value, crate::db::row::Timestamp::now()],
                )
            })
            .await?;

        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn name(raw: &str) -> Username {
        Username::parse(raw).unwrap()
    }

    #[tokio::test]
    async fn a_user_reads_back_as_created() {
        let db = db().await;

        let created = db
            .users()
            .create(NewUser {
                display_name: Some("J. Smith".into()),
                email: Some("j@example.com".into()),
                ..NewUser::person(name("j.smith"))
            })
            .await
            .unwrap();

        assert_eq!(created.username.as_str(), "j.smith");
        assert_eq!(created.kind, UserKind::Person);
        assert!(!created.is_admin);
        assert_eq!(created.created_at, created.updated_at);

        let read = db.users().get(created.id).await.unwrap().unwrap();
        assert_eq!(read, created);
    }

    #[tokio::test]
    async fn usernames_are_unique_without_regard_to_case() {
        let db = db().await;
        db.users()
            .create(NewUser::person(name("j.smith")))
            .await
            .unwrap();

        // `Username::parse` lower-cases, so a clash can only be made by writing
        // around it — which is exactly what the index has to catch.
        let clash = db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO users (username, kind, source, created_at, updated_at) \
                     VALUES ('J.Smith', 'person', 'local', '2026-01-01T00:00:00.000Z', \
                             '2026-01-01T00:00:00.000Z')",
                    [],
                )
            })
            .await;

        assert!(
            clash.is_err(),
            "the unique index should be case-insensitive"
        );
    }

    #[tokio::test]
    async fn a_user_is_found_by_name_whatever_case_is_asked_for() {
        let db = db().await;
        db.users()
            .create(NewUser::person(name("j.smith")))
            .await
            .unwrap();

        assert!(
            db.users()
                .get_by_username(&Username::from_storage("J.SMITH"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.users()
                .get_by_username(&name("someone.else"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn signing_in_again_updates_the_account_rather_than_making_another() {
        let db = db().await;

        let first = db
            .users()
            .upsert_oidc(
                "https://id.example.com",
                "sub-1",
                &name("j.smith"),
                OidcProfile {
                    display_name: Some("J. Smith".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let renamed = db
            .users()
            .upsert_oidc(
                "https://id.example.com",
                "sub-1",
                &name("jane.smith"),
                OidcProfile {
                    display_name: Some("Jane Smith".into()),
                    is_admin: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(first.id, renamed.id);
        assert_eq!(renamed.username.as_str(), "jane.smith");
        assert!(renamed.is_admin);
        assert_eq!(renamed.source, UserSource::Oidc);
        assert!(renamed.last_login_at.is_some());
        assert_eq!(db.users().list(Page::default()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_same_subject_from_another_issuer_is_another_person() {
        let db = db().await;

        let a = db
            .users()
            .upsert_oidc(
                "https://a.example",
                "sub-1",
                &name("a.user"),
                Default::default(),
            )
            .await
            .unwrap();
        let b = db
            .users()
            .upsert_oidc(
                "https://b.example",
                "sub-1",
                &name("b.user"),
                Default::default(),
            )
            .await
            .unwrap();

        assert_ne!(a.id, b.id);
        assert_eq!(
            db.users()
                .get_by_oidc("https://a.example", "sub-1")
                .await
                .unwrap()
                .unwrap()
                .id,
            a.id
        );
    }

    #[tokio::test]
    async fn the_admin_count_ignores_disabled_accounts_and_honours_the_override() {
        let db = db().await;
        let admin = db
            .users()
            .create(NewUser {
                is_admin: true,
                ..NewUser::person(name("admin"))
            })
            .await
            .unwrap();
        let other = db
            .users()
            .create(NewUser::person(name("other")))
            .await
            .unwrap();

        assert_eq!(db.users().count_admins().await.unwrap(), 1);

        assert!(
            db.users()
                .set_admin_override(other.id, Some(true))
                .await
                .unwrap()
        );
        assert_eq!(db.users().count_admins().await.unwrap(), 2);

        assert!(db.users().set_disabled(admin.id, true).await.unwrap());
        assert_eq!(db.users().count_admins().await.unwrap(), 1);

        assert!(
            db.users()
                .set_admin_override(admin.id, Some(false))
                .await
                .unwrap()
        );
        let read = db.users().get(admin.id).await.unwrap().unwrap();
        assert!(!read.is_effective_admin());
    }

    #[tokio::test]
    async fn listing_is_ordered_by_name_and_pages() {
        let db = db().await;
        for username in ["c.user", "a.user", "b.user"] {
            db.users()
                .create(NewUser::person(name(username)))
                .await
                .unwrap();
        }

        let names: Vec<String> = db
            .users()
            .list(Page::default())
            .await
            .unwrap()
            .into_iter()
            .map(|user| user.username.into_inner())
            .collect();
        assert_eq!(names, vec!["a.user", "b.user", "c.user"]);

        let page = db.users().list(Page::at(1, 1)).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].username.as_str(), "b.user");
    }

    #[tokio::test]
    async fn touching_last_seen_does_not_move_updated_at() {
        let db = db().await;
        let user = db
            .users()
            .create(NewUser::person(name("j.smith")))
            .await
            .unwrap();

        db.users().touch_last_seen(user.id).await.unwrap();

        let read = db.users().get(user.id).await.unwrap().unwrap();
        assert!(read.last_seen_at.is_some());
        assert_eq!(read.updated_at, user.updated_at);
    }

    #[tokio::test]
    async fn deleting_a_user_reports_whether_there_was_one() {
        let db = db().await;
        let user = db
            .users()
            .create(NewUser::person(name("j.smith")))
            .await
            .unwrap();

        assert!(db.users().delete(user.id).await.unwrap());
        assert!(!db.users().delete(user.id).await.unwrap());
        assert!(db.users().get(user.id).await.unwrap().is_none());
    }
}
