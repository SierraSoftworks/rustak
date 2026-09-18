//! The SQL behind the three profile tables.
//!
//! One repository borrowed from the database for the length of a call, as
//! every other aggregate here is. It lives beside the profiles rather than in
//! `db::repos` because everything that reads these rows is in this module —
//! nothing else in the server has an opinion about what a profile is.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_api::{GroupName, PrefClass, PrefEntry, ProfileId};
use rustak_core::prelude::*;

use crate::db::Database;
use crate::db::row::Timestamp;

use super::model::{COLUMNS, Delivery, FILE_COLUMNS, NewProfile, ProfileFileRow, ProfileRow};

/// Reads and writes the three profile tables.
pub struct ProfilesRepo<'a> {
    db: &'a Database,
}

impl<'a> ProfilesRepo<'a> {
    /// Borrows the repository for the length of a call.
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Every profile, newest change first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self) -> Result<Vec<ProfileRow>, Error> {
        self.db
            .read(move |c| {
                c.prepare(&format!("SELECT {COLUMNS} FROM profiles ORDER BY name ASC"))?
                    .query_map([], ProfileRow::from_row)?
                    .collect()
            })
            .await
    }

    /// One profile by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: ProfileId) -> Result<Option<ProfileRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM profiles WHERE id = ?1"),
                    [id.get()],
                    ProfileRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// One profile by name, which is how the Marti admin surface addresses it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_name(&self, name: &str) -> Result<Option<ProfileRow>, Error> {
        let name = name.to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM profiles WHERE name = ?1"),
                    [name],
                    ProfileRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// The active profiles one delivery offers, optionally only those changed
    /// since `since`.
    ///
    /// Group matching is done by the caller, which holds the channel names.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn for_delivery(
        &self,
        delivery: Delivery,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<ProfileRow>, Error> {
        let predicate = delivery.predicate();
        let tool = match &delivery {
            Delivery::Tool(tool) => Some(tool.clone()),
            _ => None,
        };
        let since = since.map(|at| Timestamp::from(at).to_text());

        self.db
            .read(move |c| {
                // The placeholder number depends on whether the tool took `?1`,
                // and a gap in the numbering is an `InvalidParameterCount`
                // rather than a query that quietly ignores it.
                let window = match (&tool, &since) {
                    (_, None) => "",
                    (Some(_), Some(_)) => " AND updated_at > ?2",
                    (None, Some(_)) => " AND updated_at > ?1",
                };
                let sql = format!(
                    "SELECT {COLUMNS} FROM profiles \
                     WHERE enabled = 1 AND {predicate}{window} ORDER BY priority DESC, name ASC"
                );

                let mut statement = c.prepare(&sql)?;
                let rows = match (tool, since) {
                    (Some(tool), Some(since)) => {
                        statement.query_map(rusqlite::params![tool, since], ProfileRow::from_row)?
                    }
                    (Some(tool), None) => statement.query_map([tool], ProfileRow::from_row)?,
                    (None, Some(since)) => statement.query_map([since], ProfileRow::from_row)?,
                    (None, None) => statement.query_map([], ProfileRow::from_row)?,
                };

                rows.collect()
            })
            .await
    }

    /// Creates a profile.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the name is taken, and a
    /// [`human_errors::Kind::System`] error if the write fails.
    pub async fn create(&self, new: NewProfile) -> Result<ProfileRow, Error> {
        let name = new.name.clone();
        let groups = group_json(&new.groups);

        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO profiles \
                         (name, description, enabled, apply_on_enrollment, apply_on_connect, \
                          tool, type, groups, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.name,
                        new.description,
                        new.active,
                        new.apply_on_enrollment,
                        new.apply_on_connect,
                        new.tool,
                        new.kind,
                        groups,
                        Timestamp::now(),
                    ],
                    ProfileRow::from_row,
                )
            })
            .await
            .map_err(|err| taken(&name, err))
    }

    /// Applies a change, touching `updated_at` so that a connected device is
    /// offered the profile again.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn update(
        &self,
        id: ProfileId,
        change: &rustak_api::ProfileUpdate,
    ) -> Result<Option<ProfileRow>, Error> {
        let change = change.clone();

        self.db
            .write(move |tx| {
                let groups = change.groups.as_deref().map(group_json);

                tx.query_one(
                    &format!(
                        "UPDATE profiles SET \
                           description = COALESCE(?2, description), \
                           enabled = COALESCE(?3, enabled), \
                           apply_on_enrollment = COALESCE(?4, apply_on_enrollment), \
                           apply_on_connect = COALESCE(?5, apply_on_connect), \
                           tool = COALESCE(?6, tool), \
                           type = COALESCE(?7, type), \
                           groups = COALESCE(?8, groups), \
                           updated_at = ?9 \
                         WHERE id = ?1 RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        id.get(),
                        change.description,
                        change.active,
                        change.apply_on_enrollment,
                        change.apply_on_connect,
                        change.tool,
                        change.kind,
                        groups,
                        Timestamp::now(),
                    ],
                    ProfileRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Deletes a profile and, by cascade, its files and preferences.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, id: ProfileId) -> Result<bool, Error> {
        let removed = self
            .db
            .write(move |tx| tx.execute("DELETE FROM profiles WHERE id = ?1", [id.get()]))
            .await?;

        Ok(removed > 0)
    }

    /// The files attached to a profile, in delivery order.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn files(&self, id: ProfileId) -> Result<Vec<ProfileFileRow>, Error> {
        self.db
            .read(move |c| {
                c.prepare(&format!(
                    "SELECT {FILE_COLUMNS} FROM profile_files WHERE profile_id = ?1 \
                     ORDER BY path ASC"
                ))?
                .query_map([id.get()], ProfileFileRow::from_row)?
                .collect()
            })
            .await
    }

    /// One file by row id, whichever profile it belongs to.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn file(&self, file_id: i64) -> Result<Option<ProfileFileRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {FILE_COLUMNS} FROM profile_files WHERE id = ?1"),
                    [file_id],
                    ProfileFileRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Attaches a file, replacing one already at that path.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn put_file(
        &self,
        id: ProfileId,
        path: String,
        hash: String,
        size: u64,
        mime_type: Option<String>,
    ) -> Result<ProfileFileRow, Error> {
        self.db
            .write(move |tx| {
                let now = Timestamp::now();
                let row = tx.query_one(
                    &format!(
                        "INSERT INTO profile_files \
                           (profile_id, path, hash, size, mime_type, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
                         ON CONFLICT (profile_id, path) DO UPDATE SET \
                           hash = excluded.hash, size = excluded.size, \
                           mime_type = excluded.mime_type, updated_at = excluded.updated_at \
                         RETURNING {FILE_COLUMNS}"
                    ),
                    rusqlite::params![id.get(), path, hash, size as i64, mime_type, now],
                    ProfileFileRow::from_row,
                )?;

                tx.execute(
                    "UPDATE profiles SET updated_at = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), now],
                )?;

                Ok(row)
            })
            .await
    }

    /// Detaches a file. The bytes stay in the content store.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete_file(&self, id: ProfileId, file_id: i64) -> Result<bool, Error> {
        let removed = self
            .db
            .write(move |tx| {
                let removed = tx.execute(
                    "DELETE FROM profile_files WHERE id = ?1 AND profile_id = ?2",
                    rusqlite::params![file_id, id.get()],
                )?;

                tx.execute(
                    "UPDATE profiles SET updated_at = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), Timestamp::now()],
                )?;

                Ok(removed)
            })
            .await?;

        Ok(removed > 0)
    }

    /// A profile's preferences, in the order they render in.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn prefs(&self, id: ProfileId) -> Result<Vec<PrefEntry>, Error> {
        self.db
            .read(move |c| {
                c.prepare(
                    "SELECT key, class, value FROM profile_prefs WHERE profile_id = ?1 \
                     ORDER BY position ASC, key ASC",
                )?
                .query_map([id.get()], |row| {
                    Ok(PrefEntry {
                        key: row.get(0)?,
                        class: crate::db::row::enum_col(row, 1, PrefClass::parse)?,
                        value: row.get(2)?,
                    })
                })?
                .collect()
            })
            .await
    }

    /// Replaces a profile's preferences wholesale.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_prefs(&self, id: ProfileId, entries: &[PrefEntry]) -> Result<(), Error> {
        let entries = entries.to_vec();

        self.db
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM profile_prefs WHERE profile_id = ?1",
                    [id.get()],
                )?;

                for (position, entry) in entries.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO profile_prefs (profile_id, key, class, value, position) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![
                            id.get(),
                            entry.key,
                            entry.class.as_str(),
                            entry.value,
                            position as i64,
                        ],
                    )?;
                }

                tx.execute(
                    "UPDATE profiles SET updated_at = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), Timestamp::now()],
                )
            })
            .await?;

        Ok(())
    }
}

/// The channel list as it is stored.
fn group_json(groups: &[GroupName]) -> String {
    let names: Vec<&str> = groups.iter().map(GroupName::as_str).collect();

    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
}

/// Turns a unique-index violation into something an operator can act on.
fn taken(name: &str, err: Error) -> Error {
    if !err.to_string().contains("UNIQUE") {
        return err;
    }

    human_errors::user(
        format!("There is already a profile called '{name}'."),
        &["Choose another name, or edit the profile that is already here."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        let db = Database::open_in_memory().await.unwrap();
        db.upgrade().await.unwrap();
        db
    }

    fn profile(name: &str) -> NewProfile {
        NewProfile {
            name: name.to_string(),
            active: true,
            apply_on_enrollment: true,
            ..NewProfile::default()
        }
    }

    #[tokio::test]
    async fn a_profile_round_trips_through_storage() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);

        let created = repo
            .create(NewProfile {
                groups: vec![GroupName::parse("Blue").unwrap()],
                tool: Some("public".to_string()),
                ..profile("Enrollment")
            })
            .await
            .unwrap();

        assert_eq!(created.name, "Enrollment");
        assert_eq!(created.groups, vec![GroupName::parse("Blue").unwrap()]);
        assert_eq!(repo.get(created.id).await.unwrap().as_ref(), Some(&created));
        assert_eq!(
            repo.get_by_name("Enrollment").await.unwrap().as_ref(),
            Some(&created),
        );
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_name_that_is_already_here_is_refused_in_words() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);

        repo.create(profile("Enrollment")).await.unwrap();
        let err = repo.create(profile("Enrollment")).await.unwrap_err();

        assert!(err.to_string().contains("already a profile"), "{err}");
    }

    #[tokio::test]
    async fn a_delivery_selects_by_flag_tool_and_window() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);

        repo.create(profile("Enrolment only")).await.unwrap();
        repo.create(NewProfile {
            apply_on_enrollment: false,
            apply_on_connect: true,
            tool: Some("Corps".to_string()),
            ..profile("Connect")
        })
        .await
        .unwrap();
        repo.create(NewProfile {
            active: false,
            ..profile("Disabled")
        })
        .await
        .unwrap();

        let enrolment = repo.for_delivery(Delivery::Enrollment, None).await.unwrap();
        assert_eq!(enrolment.len(), 1, "an inactive profile is never delivered");
        assert_eq!(enrolment[0].name, "Enrolment only");

        let connect = repo.for_delivery(Delivery::Connect, None).await.unwrap();
        assert_eq!(connect.len(), 1);

        let tool = repo
            .for_delivery(Delivery::Tool("Corps".to_string()), None)
            .await
            .unwrap();
        assert_eq!(tool.len(), 1);
        assert!(
            repo.for_delivery(Delivery::Tool("Other".to_string()), None)
                .await
                .unwrap()
                .is_empty(),
        );

        let future = Utc::now() + chrono::Duration::minutes(5);
        assert!(
            repo.for_delivery(Delivery::Enrollment, Some(future))
                .await
                .unwrap()
                .is_empty(),
            "nothing changed inside the window",
        );
    }

    #[tokio::test]
    async fn a_file_replaces_the_one_at_the_same_path_and_touches_the_profile() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);
        let created = repo.create(profile("Enrollment")).await.unwrap();

        repo.put_file(created.id, "a.pref".into(), "hash1".into(), 4, None)
            .await
            .unwrap();
        let second = repo
            .put_file(created.id, "a.pref".into(), "hash2".into(), 8, None)
            .await
            .unwrap();

        let files = repo.files(created.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hash, "hash2");
        assert_eq!(repo.file(second.id).await.unwrap().unwrap().size, 8);

        let touched = repo.get(created.id).await.unwrap().unwrap();
        assert!(touched.updated_at >= created.updated_at);

        assert!(repo.delete_file(created.id, second.id).await.unwrap());
        assert!(repo.files(created.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn preferences_keep_the_order_they_were_written_in() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);
        let created = repo.create(profile("Enrollment")).await.unwrap();

        let entries = vec![
            PrefEntry::string("zulu", "1"),
            PrefEntry::new("alpha", PrefClass::Integer, "2"),
        ];
        repo.set_prefs(created.id, &entries).await.unwrap();

        assert_eq!(repo.prefs(created.id).await.unwrap(), entries);

        repo.set_prefs(created.id, &[]).await.unwrap();
        assert!(repo.prefs(created.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_profile_takes_its_files_and_preferences_with_it() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);
        let created = repo.create(profile("Enrollment")).await.unwrap();

        repo.put_file(created.id, "a.pref".into(), "h".into(), 1, None)
            .await
            .unwrap();
        repo.set_prefs(created.id, &[PrefEntry::string("a", "b")])
            .await
            .unwrap();

        assert!(repo.delete(created.id).await.unwrap());
        assert!(!repo.delete(created.id).await.unwrap());
        assert!(repo.files(created.id).await.unwrap().is_empty());
        assert!(repo.prefs(created.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_update_leaves_the_fields_it_was_not_given_alone() {
        let db = db().await;
        let repo = ProfilesRepo::new(&db);
        let created = repo
            .create(NewProfile {
                tool: Some("public".to_string()),
                ..profile("Enrollment")
            })
            .await
            .unwrap();

        let updated = repo
            .update(
                created.id,
                &rustak_api::ProfileUpdate {
                    active: Some(false),
                    ..rustak_api::ProfileUpdate::default()
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!updated.active);
        assert_eq!(updated.tool.as_deref(), Some("public"));
        assert!(updated.apply_on_enrollment);
    }

    #[test]
    fn an_empty_channel_list_means_everybody() {
        let row = ProfileRow {
            id: ProfileId::new(1),
            name: "P".to_string(),
            description: None,
            active: true,
            apply_on_enrollment: true,
            apply_on_connect: false,
            tool: None,
            kind: None,
            groups: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(row.visible_to(&[]));

        let scoped = ProfileRow {
            groups: vec![GroupName::parse("Blue").unwrap()],
            ..row
        };

        assert!(!scoped.visible_to(&[]));
        assert!(scoped.visible_to(&[GroupName::parse("Blue").unwrap()]));
        assert!(!scoped.visible_to(&[GroupName::parse("Red").unwrap()]));
    }
}
