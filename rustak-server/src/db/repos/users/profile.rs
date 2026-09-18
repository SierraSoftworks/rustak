//! The two fields of an account an administrator types rather than decides.
//!
//! A child module rather than more lines in [`super`], which is at
//! `conventions.md`'s length limit. The split is between the flags that change
//! what an account *may do* — disabled, the administrative override — and the
//! description of who it belongs to, which is here.
//!
//! # Why the statement carries a flag per field
//!
//! Absent, `null` and a value are three different instructions, and only the
//! caller knows which one was sent. `COALESCE(?2, display_name)` cannot express
//! "set it to nothing", so each field is bound twice: whether it is being
//! written at all, and what to write. A patch that names neither field never
//! reaches here.

use rustak_core::prelude::*;

use crate::db::row::Timestamp;

use super::UsersRepo;

/// What an administrator is changing about who an account belongs to.
///
/// [`None`] leaves a field alone; `Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileChange {
    pub display_name: Option<Option<String>>,
    pub email: Option<Option<String>>,
}

impl ProfileChange {
    /// Whether this would write anything.
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none() && self.email.is_none()
    }
}

impl UsersRepo<'_> {
    /// Sets the display name and email, reporting whether a row changed.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_profile(&self, id: UserId, change: ProfileChange) -> Result<bool, Error> {
        if change.is_empty() {
            return Ok(false);
        }

        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE users SET \
                       display_name = CASE WHEN ?2 THEN ?3 ELSE display_name END, \
                       email = CASE WHEN ?4 THEN ?5 ELSE email END, \
                       updated_at = ?6 \
                     WHERE id = ?1",
                    rusqlite::params![
                        id.get(),
                        change.display_name.is_some(),
                        change.display_name.flatten(),
                        change.email.is_some(),
                        change.email.flatten(),
                        Timestamp::now(),
                    ],
                )
            })
            .await?;

        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use crate::db::repos::NewUser;

    use super::*;

    async fn seeded() -> (Database, UserId) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser {
                display_name: Some("Grace Hopper".into()),
                email: Some("grace@example.com".into()),
                ..NewUser::person(Username::parse("grace").unwrap())
            })
            .await
            .unwrap();

        (db, user.id)
    }

    #[tokio::test]
    async fn a_field_that_was_not_named_is_left_alone() {
        let (db, id) = seeded().await;

        assert!(
            db.users()
                .set_profile(
                    id,
                    ProfileChange {
                        display_name: Some(Some("Rear Admiral Hopper".into())),
                        email: None,
                    },
                )
                .await
                .unwrap()
        );

        let row = db.users().get(id).await.unwrap().unwrap();
        assert_eq!(row.display_name.as_deref(), Some("Rear Admiral Hopper"));
        assert_eq!(row.email.as_deref(), Some("grace@example.com"));
    }

    #[tokio::test]
    async fn a_field_named_as_nothing_is_cleared() {
        let (db, id) = seeded().await;

        db.users()
            .set_profile(
                id,
                ProfileChange {
                    display_name: None,
                    email: Some(None),
                },
            )
            .await
            .unwrap();

        let row = db.users().get(id).await.unwrap().unwrap();
        assert_eq!(row.email, None);
        assert_eq!(row.display_name.as_deref(), Some("Grace Hopper"));
    }

    #[tokio::test]
    async fn a_change_that_names_nothing_writes_nothing() {
        let (db, id) = seeded().await;
        let before = db.users().get(id).await.unwrap().unwrap();

        assert!(
            !db.users()
                .set_profile(id, ProfileChange::default())
                .await
                .unwrap()
        );
        assert_eq!(db.users().get(id).await.unwrap().unwrap(), before);
    }

    #[tokio::test]
    async fn an_account_that_is_not_here_reports_that_nothing_changed() {
        let (db, _) = seeded().await;

        assert!(
            !db.users()
                .set_profile(
                    UserId::new(9999),
                    ProfileChange {
                        email: Some(None),
                        ..ProfileChange::default()
                    },
                )
                .await
                .unwrap()
        );
    }
}
