//! One account's preferences, a row per key.
//!
//! The keys and what their values mean belong to
//! [`rustak_api::preferences`]; this is only where they are kept. A key nobody
//! has set has no row and reads as its default, and a value this build does not
//! recognise reads the same way — a row written by a newer server must never be
//! the reason an older one cannot answer `/me`.

use chrono::{DateTime, Utc};
use rustak_api::{Symbology, UserPreferences, UserPreferencesPatch};
use rustak_core::prelude::*;

use crate::db::{Database, row::Timestamp};

/// The key [`UserPreferences::symbology`] is stored under.
const SYMBOLOGY: &str = "symbology";

/// Reads and writes `user_preferences`.
pub struct UserPreferencesRepo<'a> {
    db: &'a Database,
}

impl<'a> UserPreferencesRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Everything one account has chosen, with the default for what it has not.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, user_id: UserId) -> Result<UserPreferences, Error> {
        let rows: Vec<(String, String)> = self
            .db
            .read(move |c| {
                c.prepare("SELECT key, value FROM user_preferences WHERE user_id = ?1")?
                    .query_map([user_id.get()], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .await?;

        let mut preferences = UserPreferences::default();
        for (key, value) in rows {
            if key == SYMBOLOGY {
                preferences.symbology = Symbology::parse(&value).unwrap_or_default();
            }
        }

        Ok(preferences)
    }

    /// Only what the account has actually chosen, and when it last chose, or
    /// [`None`] for an account that has chosen nothing.
    ///
    /// A default is what *this console* assumes; it is not something the person
    /// decided, so it is not something to go and configure their devices with.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn chosen(
        &self,
        user_id: UserId,
    ) -> Result<Option<(UserPreferencesPatch, DateTime<Utc>)>, Error> {
        let rows: Vec<(String, String, Timestamp)> = self
            .db
            .read(move |c| {
                c.prepare("SELECT key, value, updated_at FROM user_preferences WHERE user_id = ?1")?
                    .query_map([user_id.get()], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect()
            })
            .await?;

        let mut chosen = UserPreferencesPatch::default();
        let mut newest = None::<DateTime<Utc>>;

        for (key, value, at) in rows {
            if key == SYMBOLOGY {
                chosen.symbology = Symbology::parse(&value);
            }
            newest = newest.max(Some(at.into()));
        }

        Ok(newest.filter(|_| !chosen.is_empty()).map(|at| (chosen, at)))
    }

    /// Sets what `patch` names and leaves the rest alone. Answers the whole.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails, including
    /// when the account does not exist.
    pub async fn apply(
        &self,
        user_id: UserId,
        patch: UserPreferencesPatch,
    ) -> Result<UserPreferences, Error> {
        let changes: Vec<(&'static str, &'static str)> = patch
            .symbology
            .map(|symbology| (SYMBOLOGY, symbology.as_str()))
            .into_iter()
            .collect();

        self.db
            .write(move |tx| {
                for (key, value) in changes {
                    tx.execute(
                        "INSERT INTO user_preferences (user_id, key, value, updated_at) \
                         VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT (user_id, key) \
                         DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        rusqlite::params![user_id.get(), key, value, Timestamp::now()],
                    )?;
                }

                Ok(())
            })
            .await?;

        self.get(user_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    async fn account(db: &Database, name: &str) -> UserId {
        db.users()
            .create(NewUser::person(Username::parse(name).unwrap()))
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn an_account_that_has_chosen_nothing_has_the_defaults() {
        let db = Database::open_in_memory().await.unwrap();
        let ada = account(&db, "ada").await;

        assert_eq!(
            db.user_preferences().get(ada).await.unwrap(),
            UserPreferences::default()
        );
    }

    #[tokio::test]
    async fn a_choice_belongs_to_one_account_and_can_be_changed_back() {
        let db = Database::open_in_memory().await.unwrap();
        let (ada, grace) = (account(&db, "ada").await, account(&db, "grace").await);
        let choose = |symbology| UserPreferencesPatch {
            symbology: Some(symbology),
        };

        let chosen = db
            .user_preferences()
            .apply(ada, choose(Symbology::Milstd2525D))
            .await
            .unwrap();

        assert_eq!(chosen.symbology, Symbology::Milstd2525D);
        assert_eq!(
            db.user_preferences().get(ada).await.unwrap().symbology,
            Symbology::Milstd2525D
        );
        assert_eq!(
            db.user_preferences().get(grace).await.unwrap().symbology,
            Symbology::Milstd2525C,
            "one account's choice is not another's",
        );

        let back = db
            .user_preferences()
            .apply(ada, choose(Symbology::Milstd2525C))
            .await
            .unwrap();
        assert_eq!(back.symbology, Symbology::Milstd2525C);
    }

    #[tokio::test]
    async fn a_value_this_build_has_never_heard_of_reads_as_the_default() {
        let db = Database::open_in_memory().await.unwrap();
        let ada = account(&db, "ada").await;

        db.write(move |tx| {
            tx.execute(
                "INSERT INTO user_preferences (user_id, key, value, updated_at) \
                 VALUES (?1, 'symbology', 'app6e', ?2), (?1, 'a-key-from-the-future', 'x', ?2)",
                rusqlite::params![ada.get(), Timestamp::now()],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(
            db.user_preferences().get(ada).await.unwrap(),
            UserPreferences::default()
        );
    }

    #[tokio::test]
    async fn deleting_an_account_takes_its_preferences_with_it() {
        let db = Database::open_in_memory().await.unwrap();
        let ada = account(&db, "ada").await;
        db.user_preferences()
            .apply(
                ada,
                UserPreferencesPatch {
                    symbology: Some(Symbology::Milstd2525D),
                },
            )
            .await
            .unwrap();

        db.users().delete(ada).await.unwrap();

        let left: i64 = db
            .read(|c| {
                c.query_row("SELECT COUNT(*) FROM user_preferences", [], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn only_what_was_actually_chosen_is_reported_as_chosen() {
        let db = Database::open_in_memory().await.expect("a database");
        let user = account(&db, "alice").await;
        let repo = db.user_preferences();

        assert_eq!(repo.chosen(user).await.expect("a read"), None);

        repo.apply(
            user,
            UserPreferencesPatch {
                symbology: Some(Symbology::Milstd2525D),
            },
        )
        .await
        .expect("a write");

        let (chosen, _) = repo.chosen(user).await.expect("a read").expect("a choice");
        assert_eq!(chosen.symbology, Some(Symbology::Milstd2525D));
    }
}
