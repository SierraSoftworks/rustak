//! `resources`: the metadata rows behind Enterprise Sync.
//!
//! A row here says what a stored blob *is* — its name, who submitted it, which
//! channels may see it, the keywords a client searches by — while the bytes
//! themselves live in the content-addressed store under
//! [`ResourceRow::hash`]. Nothing in this file touches the filesystem; the two
//! halves are joined by [`crate::files`].
//!
//! # Why keywords are a child table
//!
//! `?keywords=missionpackage` is one of the two questions asked of this table
//! (the other is "by hash"), and it is asked by every client's package browser
//! on every refresh. A delimited string would make that a scan with a `LIKE`
//! that cannot use an index and that matches `missionpackage-old` as well.
//!
//! # A hash is not unique and a UID is
//!
//! Two rows may share a hash: the same photograph attached to two map items is
//! two resources over one blob. A UID is how a client addresses a resource, so
//! uploading against one that already exists replaces that row rather than
//! adding a second — see [`ResourcesRepo::upsert`].

pub mod row;

use rusqlite::{OptionalExtension as _, params_from_iter};
use rustak_core::prelude::*;

use crate::db::{Database, row::Timestamp};

pub use row::{MutableField, NewResource, ResourceFilter, ResourceRow};

use row::COLUMNS;

/// Reads and writes `resources`.
pub struct ResourcesRepo<'a> {
    db: &'a Database,
}

impl<'a> ResourcesRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Stores a resource, replacing any row that already holds its UID.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn upsert(&self, new: NewResource) -> Result<ResourceRow, Error> {
        self.db
            .write(move |tx| {
                let groups = serde_json::to_string(&new.groups).unwrap_or_else(|_| "[]".into());
                let mut row = tx.query_one(
                    &format!(
                        "INSERT INTO resources (hash, uid, name, filename, mime_type, size, tool, \
                           creator_uid, submitter_id, submitter, submission_time, expiration, \
                           is_mission_package, groups, mission_name, latitude, longitude, \
                           altitude, remarks, permissions, contacts, download_path, \
                           plugin_class_name, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?11) \
                         ON CONFLICT (uid) DO UPDATE SET \
                           hash = excluded.hash, name = excluded.name, \
                           filename = excluded.filename, mime_type = excluded.mime_type, \
                           size = excluded.size, tool = excluded.tool, \
                           creator_uid = excluded.creator_uid, \
                           submitter_id = excluded.submitter_id, \
                           submitter = excluded.submitter, \
                           submission_time = excluded.submission_time, \
                           expiration = excluded.expiration, \
                           is_mission_package = excluded.is_mission_package, \
                           groups = excluded.groups, mission_name = excluded.mission_name, \
                           latitude = excluded.latitude, longitude = excluded.longitude, \
                           altitude = excluded.altitude, remarks = excluded.remarks, \
                           permissions = excluded.permissions, contacts = excluded.contacts, \
                           download_path = excluded.download_path, \
                           plugin_class_name = excluded.plugin_class_name, \
                           deleted_at = NULL \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.hash,
                        new.uid,
                        new.name,
                        new.filename,
                        new.mime_type,
                        new.size,
                        new.tool,
                        new.creator_uid,
                        new.submitter_id.map(UserId::get),
                        new.submitter,
                        Timestamp::now(),
                        new.expiration,
                        i64::from(new.is_mission_package),
                        groups,
                        new.mission_name,
                        new.latitude,
                        new.longitude,
                        new.altitude,
                        new.remarks,
                        new.permissions,
                        new.contacts,
                        new.download_path,
                        new.plugin_class_name,
                    ],
                    ResourceRow::from_row,
                )?;

                write_keywords(tx, row.id, &new.keywords)?;
                row.keywords = new.keywords;

                Ok(row)
            })
            .await
    }

    /// The rows matching a filter, keywords included.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, filter: ResourceFilter) -> Result<Vec<ResourceRow>, Error> {
        self.db
            .read(move |c| {
                let (where_sql, binds) = filter.clauses();
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM resources WHERE {where_sql} {}",
                    filter.tail()
                ))?;
                let mut rows: Vec<ResourceRow> = statement
                    .query_map(params_from_iter(binds.iter()), ResourceRow::from_row)?
                    .collect::<rusqlite::Result<_>>()?;

                for row in &mut rows {
                    row.keywords = read_keywords(c, row.id)?;
                }

                Ok(rows)
            })
            .await
    }

    /// How many rows match, ignoring `limit` and `offset`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn count(&self, filter: ResourceFilter) -> Result<i64, Error> {
        self.db
            .read(move |c| {
                let (where_sql, binds) = filter.clauses();

                c.query_one(
                    &format!("SELECT COUNT(*) FROM resources WHERE {where_sql}"),
                    params_from_iter(binds.iter()),
                    |row| row.get(0),
                )
            })
            .await
    }

    /// The most recently submitted row with this hash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_hash(&self, hash: &str) -> Result<Option<ResourceRow>, Error> {
        Ok(self
            .list(ResourceFilter {
                limit: Some(1),
                ..ResourceFilter::by_hash(hash)
            })
            .await?
            .pop())
    }

    /// Reads one row back by primary key.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_id(&self, id: i64) -> Result<Option<ResourceRow>, Error> {
        self.db
            .read(move |c| {
                let found = c
                    .query_one(
                        &format!("SELECT {COLUMNS} FROM resources WHERE id = ?1"),
                        [id],
                        ResourceRow::from_row,
                    )
                    .optional()?;

                match found {
                    Some(mut row) => {
                        row.keywords = read_keywords(c, row.id)?;
                        Ok(Some(row))
                    }
                    None => Ok(None),
                }
            })
            .await
    }

    /// The row holding this UID.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_uid(&self, uid: &str) -> Result<Option<ResourceRow>, Error> {
        Ok(self
            .list(ResourceFilter {
                uid: Some(uid.to_string()),
                ..ResourceFilter::default()
            })
            .await?
            .pop())
    }

    /// Changes one of the two mutable fields on every row with this hash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_field(
        &self,
        hash: &str,
        field: MutableField,
        value: String,
    ) -> Result<usize, Error> {
        let hash = hash.to_string();

        self.db
            .write(move |tx| {
                tx.execute(
                    &format!(
                        "UPDATE resources SET {} = ?2 WHERE hash = ?1 AND deleted_at IS NULL",
                        field.column()
                    ),
                    rusqlite::params![hash, value],
                )
            })
            .await
    }

    /// Replaces the keywords of every row with this hash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_keywords(&self, hash: &str, keywords: Vec<String>) -> Result<usize, Error> {
        let hash = hash.to_string();

        self.db
            .write(move |tx| {
                let ids = ids_for_hash(tx, &hash)?;

                for id in &ids {
                    write_keywords(tx, *id, &keywords)?;
                }

                Ok(ids.len())
            })
            .await
    }

    /// Sets, or clears, the expiry of every row with this hash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_expiration(&self, hash: &str, at: Option<i64>) -> Result<usize, Error> {
        let hash = hash.to_string();

        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE resources SET expiration = ?2 \
                     WHERE hash = ?1 AND deleted_at IS NULL",
                    rusqlite::params![hash, at],
                )
            })
            .await
    }

    /// Changes the submitter recorded against every row with this hash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_submitter(&self, hash: &str, submitter: String) -> Result<usize, Error> {
        let hash = hash.to_string();

        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE resources SET submitter = ?2 \
                     WHERE hash = ?1 AND deleted_at IS NULL",
                    rusqlite::params![hash, submitter],
                )
            })
            .await
    }

    /// Removes rows by primary key, returning how many went.
    ///
    /// A hard delete: the row is what a client browses, and a soft-deleted one
    /// would keep answering `/Marti/sync/missionquery` with a URL that no
    /// longer downloads. The audit log is what keeps the record.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, ids: Vec<i64>) -> Result<usize, Error> {
        if ids.is_empty() {
            return Ok(0);
        }

        self.db
            .write(move |tx| {
                let places = vec!["?"; ids.len()].join(", ");

                tx.execute(
                    &format!("DELETE FROM resources WHERE id IN ({places})"),
                    params_from_iter(ids.iter()),
                )
            })
            .await
    }

    /// Whether any surviving row, mission attachment or profile file still
    /// points at this blob.
    ///
    /// Asked before the bytes are removed: a hash backs as many rows as
    /// somebody has uploaded the same file under, and deleting one of them must
    /// not break the others.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn hash_in_use(&self, hash: &str) -> Result<bool, Error> {
        let hash = hash.to_string();

        self.db
            .read(move |c| {
                c.query_one(
                    "SELECT EXISTS (SELECT 1 FROM resources WHERE hash = ?1) \
                       OR EXISTS (SELECT 1 FROM profile_files WHERE hash = ?1) \
                       OR EXISTS (SELECT 1 FROM mission_contents mc \
                                  JOIN resources r ON r.id = mc.resource_id \
                                  WHERE r.hash = ?1)",
                    [hash],
                    |row| row.get::<_, i64>(0).map(|held| held != 0),
                )
            })
            .await
    }
}

/// The primary keys of every live row with this hash.
fn ids_for_hash(tx: &rusqlite::Transaction<'_>, hash: &str) -> rusqlite::Result<Vec<i64>> {
    let mut statement =
        tx.prepare("SELECT id FROM resources WHERE hash = ?1 AND deleted_at IS NULL")?;
    let ids = statement.query_map([hash], |row| row.get(0))?;

    ids.collect()
}

/// Replaces one resource's keywords.
fn write_keywords(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    keywords: &[String],
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM resource_keywords WHERE resource_id = ?1", [id])?;

    for keyword in keywords {
        tx.execute(
            "INSERT OR IGNORE INTO resource_keywords (resource_id, keyword) VALUES (?1, ?2)",
            rusqlite::params![id, keyword],
        )?;
    }

    Ok(())
}

/// One resource's keywords, in a stable order.
fn read_keywords(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<Vec<String>> {
    let mut statement = c.prepare(
        "SELECT keyword FROM resource_keywords WHERE resource_id = ?1 ORDER BY keyword ASC",
    )?;
    let keywords = statement.query_map([id], |row| row.get(0))?;

    keywords.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory()
            .await
            .expect("an in-memory database")
    }

    fn sample(uid: &str, hash: &str) -> NewResource {
        NewResource {
            hash: hash.to_string(),
            uid: uid.to_string(),
            name: "package.zip".to_string(),
            mime_type: "application/x-zip-compressed".to_string(),
            size: 12,
            tool: "public".to_string(),
            submitter: Some("grace".to_string()),
            groups: vec!["Blue".to_string()],
            keywords: vec!["missionpackage".to_string(), "interop".to_string()],
            ..NewResource::default()
        }
    }

    #[tokio::test]
    async fn a_resource_round_trips_with_its_keywords() {
        let db = db().await;

        let stored = db.resources().upsert(sample("uid-1", "aa")).await.unwrap();

        assert_eq!(stored.uid, "uid-1");
        assert_eq!(stored.groups, vec!["Blue".to_string()]);
        assert!(stored.id > 0, "the primary key is what clients parse");

        let read = db.resources().by_id(stored.id).await.unwrap().unwrap();

        assert_eq!(read.keywords, vec!["interop", "missionpackage"]);
        assert_eq!(read.hash, "aa");
    }

    #[tokio::test]
    async fn two_resources_may_share_one_blob() {
        // The same photograph attached to two map items is two resources over
        // one set of bytes; 0005's unique index made that a failure.
        let db = db().await;

        db.resources().upsert(sample("uid-1", "aa")).await.unwrap();
        db.resources().upsert(sample("uid-2", "aa")).await.unwrap();

        let both = db
            .resources()
            .list(ResourceFilter::by_hash("aa"))
            .await
            .unwrap();

        assert_eq!(both.len(), 2);
    }

    #[tokio::test]
    async fn uploading_against_an_existing_uid_replaces_that_row() {
        let db = db().await;
        db.resources().upsert(sample("uid-1", "aa")).await.unwrap();

        let second = db
            .resources()
            .upsert(NewResource {
                name: "renamed.zip".to_string(),
                keywords: vec!["other".to_string()],
                ..sample("uid-1", "bb")
            })
            .await
            .unwrap();

        assert_eq!(second.hash, "bb");
        assert_eq!(second.name, "renamed.zip");
        assert_eq!(
            db.resources()
                .list(ResourceFilter::default())
                .await
                .unwrap()
                .len(),
            1,
        );
        assert_eq!(
            db.resources()
                .by_id(second.id)
                .await
                .unwrap()
                .unwrap()
                .keywords,
            vec!["other"],
            "the keywords are replaced rather than merged",
        );
    }

    #[tokio::test]
    async fn a_filter_is_every_clause_at_once() {
        let db = db().await;
        db.resources().upsert(sample("uid-1", "aa")).await.unwrap();
        db.resources()
            .upsert(NewResource {
                tool: "private".to_string(),
                keywords: vec!["other".to_string()],
                ..sample("uid-2", "bb")
            })
            .await
            .unwrap();

        let packages = db
            .resources()
            .list(ResourceFilter {
                keywords: vec!["missionpackage".to_string()],
                tool: Some("public".to_string()),
                ..ResourceFilter::default()
            })
            .await
            .unwrap();

        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].uid, "uid-1");
        assert_eq!(
            db.resources()
                .count(ResourceFilter::default())
                .await
                .unwrap(),
            2,
        );
    }

    #[tokio::test]
    async fn the_mutable_fields_are_the_only_ones_that_move() {
        let db = db().await;
        let stored = db.resources().upsert(sample("uid-1", "aa")).await.unwrap();

        db.resources()
            .set_field("aa", MutableField::Tool, "private".to_string())
            .await
            .unwrap();
        db.resources()
            .set_keywords("aa", vec!["replaced".to_string()])
            .await
            .unwrap();
        db.resources().set_expiration("aa", Some(42)).await.unwrap();

        let read = db.resources().by_id(stored.id).await.unwrap().unwrap();

        assert_eq!(read.tool, "private");
        assert_eq!(read.keywords, vec!["replaced"]);
        assert_eq!(read.expiration, Some(42));
        assert_eq!(read.name, stored.name, "a name is never mutable");
    }

    #[tokio::test]
    async fn a_blob_is_in_use_until_its_last_row_goes() {
        let db = db().await;
        let first = db.resources().upsert(sample("uid-1", "aa")).await.unwrap();
        let second = db.resources().upsert(sample("uid-2", "aa")).await.unwrap();

        assert_eq!(db.resources().delete(vec![first.id]).await.unwrap(), 1);
        assert!(db.resources().hash_in_use("aa").await.unwrap());

        db.resources().delete(vec![second.id]).await.unwrap();

        assert!(!db.resources().hash_in_use("aa").await.unwrap());
    }
}
