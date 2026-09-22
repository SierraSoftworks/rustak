//! One account's preferences, a row per key.
//!
//! The keys and what their values mean belong to
//! [`rustak_api::preferences`]; this is only where they are kept. A key nobody
//! has set has no row and reads as its default, and a value this build does not
//! recognise reads the same way — a row written by a newer server must never be
//! the reason an older one cannot answer `/me`.

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
    async fn a_choice_is_kept_is_the_accounts_own_and_can_be_changed_back() {
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
}
