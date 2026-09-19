//! The two ways an identity provider comes to own an account.
//!
//! A child module rather than more lines in [`super`], which is at
//! `conventions.md`'s length limit.
//!
//! [`UsersRepo::upsert_oidc`] keys on the provider's subject: it inserts a
//! stranger and refreshes somebody it has seen before. That is the wrong write
//! for a person who already has an account here under the name the provider
//! vouches for — the insert trips the unique index on `username` — which is
//! what [`UsersRepo::link_oidc`] is for.

use rustak_core::prelude::*;

use crate::db::row::Timestamp;

use super::{COLUMNS, UserRow, UsersRepo};

/// What an identity provider told us about somebody.
#[derive(Debug, Clone, Default)]
pub struct OidcProfile {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_admin: bool,
    /// The provider's filterable claims, serialised, for the access-control
    /// expressions to be evaluated against on later requests.
    pub claims: Option<String>,
}

impl UsersRepo<'_> {
    /// Creates or refreshes the account behind an identity-provider subject.
    ///
    /// The subject is the identity, not the username: a provider that lets
    /// somebody rename themselves must not strand their account or let them
    /// walk into another one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails — including
    /// when the provider's name for a stranger is already somebody else's
    /// username, which the caller is expected to have checked.
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
                let now = Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO users \
                           (username, kind, display_name, email, is_admin, source, \
                            oidc_issuer, oidc_subject, oidc_claims, \
                            created_at, updated_at, last_login_at) \
                         VALUES (?1, 'person', ?2, ?3, ?4, 'oidc', ?5, ?6, ?7, ?8, ?8, ?8) \
                         ON CONFLICT (oidc_issuer, oidc_subject) \
                           WHERE oidc_subject IS NOT NULL DO UPDATE SET \
                           username = excluded.username, \
                           display_name = excluded.display_name, \
                           email = excluded.email, \
                           is_admin = excluded.is_admin, \
                           source = 'oidc', \
                           oidc_claims = excluded.oidc_claims, \
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
                        profile.claims,
                        now,
                    ],
                    UserRow::from_row,
                )
            })
            .await
    }

    /// Binds an existing account to an identity-provider subject.
    ///
    /// From here on the row is a provider-backed account: its source, subject,
    /// name and profile are the provider's, and [`Self::upsert_oidc`] finds it
    /// by subject on every later sign-in. What the account *had* is kept — its
    /// passkeys, credentials, devices, channels and administrative override —
    /// because the point of linking is that the person keeps their account.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails, or if `id`
    /// names nobody.
    pub async fn link_oidc(
        &self,
        id: UserId,
        issuer: &str,
        subject: &str,
        username: &Username,
        profile: OidcProfile,
    ) -> Result<UserRow, Error> {
        let (issuer, subject) = (issuer.to_owned(), subject.to_owned());
        let username = username.as_str().to_owned();

        self.db
            .write(move |tx| {
                let now = Timestamp::now();

                tx.query_one(
                    &format!(
                        "UPDATE users SET \
                           username = ?2, \
                           display_name = ?3, \
                           email = ?4, \
                           is_admin = ?5, \
                           source = 'oidc', \
                           oidc_issuer = ?6, \
                           oidc_subject = ?7, \
                           oidc_claims = ?8, \
                           updated_at = ?9, \
                           last_login_at = ?9 \
                         WHERE id = ?1 \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        id.get(),
                        username,
                        profile.display_name,
                        profile.email,
                        i64::from(profile.is_admin),
                        issuer,
                        subject,
                        profile.claims,
                        now,
                    ],
                    UserRow::from_row,
                )
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::UserSource;

    use crate::db::Database;
    use crate::db::repos::NewUser;

    use super::*;

    const ISSUER: &str = "https://id.example.com";

    async fn seeded() -> (Database, UserRow) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser {
                display_name: Some("Grace Hopper".into()),
                email: Some("grace@example.com".into()),
                is_admin: true,
                ..NewUser::person(Username::parse("grace").unwrap())
            })
            .await
            .unwrap();

        (db, user)
    }

    fn profile() -> OidcProfile {
        OidcProfile {
            display_name: Some("Rear Admiral Hopper".into()),
            email: Some("hopper@example.com".into()),
            is_admin: false,
            claims: Some(r#"{"groups":["navy"]}"#.into()),
        }
    }

    #[tokio::test]
    async fn a_sign_in_keeps_the_claims_it_was_judged_by() {
        let db = Database::open_in_memory().await.unwrap();

        let row = db
            .users()
            .upsert_oidc(
                ISSUER,
                "subject-1",
                &Username::parse("grace").unwrap(),
                profile(),
            )
            .await
            .unwrap();
        assert_eq!(row.oidc_claims.as_deref(), Some(r#"{"groups":["navy"]}"#));

        let again = db
            .users()
            .upsert_oidc(
                ISSUER,
                "subject-1",
                &Username::parse("grace").unwrap(),
                OidcProfile {
                    claims: Some(r#"{"groups":[]}"#.into()),
                    ..profile()
                },
            )
            .await
            .unwrap();
        assert_eq!(again.id, row.id);
        assert_eq!(
            again.oidc_claims.as_deref(),
            Some(r#"{"groups":[]}"#),
            "the next sign-in replaces them rather than keeping the first"
        );
    }

    #[tokio::test]
    async fn linking_makes_the_row_the_providers_and_keeps_its_id() {
        let (db, before) = seeded().await;

        let linked = db
            .users()
            .link_oidc(before.id, ISSUER, "subject-1", &before.username, profile())
            .await
            .unwrap();

        assert_eq!(linked.id, before.id);
        assert_eq!(linked.source, UserSource::Oidc);
        assert_eq!(linked.oidc_issuer.as_deref(), Some(ISSUER));
        assert_eq!(linked.oidc_subject.as_deref(), Some("subject-1"));
        assert_eq!(
            linked.oidc_claims.as_deref(),
            Some(r#"{"groups":["navy"]}"#)
        );
        assert_eq!(linked.display_name.as_deref(), Some("Rear Admiral Hopper"));
        assert!(!linked.is_admin, "the flag is now the sign-in's decision");
        assert!(linked.last_login_at.is_some());

        let by_subject = db
            .users()
            .get_by_oidc(ISSUER, "subject-1")
            .await
            .unwrap()
            .expect("the subject now finds the account");
        assert_eq!(by_subject.id, before.id);
    }

    #[tokio::test]
    async fn a_later_sign_in_finds_the_linked_row_by_subject() {
        let (db, before) = seeded().await;

        db.users()
            .link_oidc(before.id, ISSUER, "subject-1", &before.username, profile())
            .await
            .unwrap();

        let again = db
            .users()
            .upsert_oidc(
                ISSUER,
                "subject-1",
                &Username::parse("grace.h").unwrap(),
                profile(),
            )
            .await
            .unwrap();

        assert_eq!(
            again.id, before.id,
            "the upsert updated rather than inserted"
        );
        assert_eq!(again.username.as_str(), "grace.h");
    }

    #[tokio::test]
    async fn nobody_to_link_is_an_error_rather_than_a_silent_no_op() {
        let (db, _) = seeded().await;

        let missing = db
            .users()
            .link_oidc(
                UserId::new(9_999),
                ISSUER,
                "subject-1",
                &Username::parse("nobody").unwrap(),
                profile(),
            )
            .await;

        assert!(missing.is_err());
    }
}
