//! `missions` and its four child tables: the storage behind Data Sync.
//!
//! Four repositories rather than one, because the aggregates are written on
//! very different paths — a mission is created once and edited rarely, its
//! change log is append-only and read by time window, its contents are upserted
//! by whoever publishes into it, and its subscriptions come and go with every
//! reconnect. They share a module so that the row types and the SQL that reads
//! them stay next to each other.
//!
//! # Deleting a mission does not delete the row
//!
//! `deleted_at` is set instead, because a client that still holds a mission
//! token must be told `410 Gone` rather than `404 Not Found` — the difference
//! between "stop asking" and "try the other spelling". Every listing filters
//! deleted rows out; only the two `by_*` resolvers return them, so that the
//! route layer can tell the two answers apart.

pub mod changes;
pub mod contents;
pub mod row;
pub mod subscriptions;

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{OptionalExtension as _, params_from_iter};
use rustak_core::prelude::*;
use uuid::Uuid;

use crate::db::{Database, row::Timestamp, row::to_json};

pub use row::{MissionFilter, MissionPatch, MissionRow, NewMission};

use row::COLUMNS;

/// Reads and writes `missions`.
pub struct MissionsRepo<'a> {
    db: &'a Database,
}

impl<'a> MissionsRepo<'a> {
    pub(in crate::db) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Stores a new mission.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails, which for a
    /// name that is already taken is a unique-index violation the caller turns
    /// into a duplicate.
    pub async fn create(&self, new: NewMission) -> Result<MissionRow, Error> {
        self.db
            .write(move |tx| {
                let now = Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO missions (guid, name, description, chat_room, base_layer, \
                           bbox, bounding_polygon, path, classification, tool, keywords, \
                           creator_uid, create_time, default_role, invite_only, password_hash, \
                           expiration, groups, parent_id, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                                 ?15, ?16, ?17, ?18, ?19, ?13, ?13) \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.guid.to_string(),
                        new.name,
                        new.description,
                        new.chat_room,
                        new.base_layer,
                        new.bbox,
                        to_json(&new.bounding_polygon)?,
                        new.path,
                        new.classification,
                        new.tool,
                        to_json(&new.keywords)?,
                        new.creator_uid,
                        now,
                        new.default_role,
                        i64::from(new.invite_only),
                        new.password_hash,
                        new.expiration,
                        to_json(&new.groups)?,
                        new.parent_id,
                    ],
                    MissionRow::from_row,
                )
            })
            .await
    }

    /// The missions matching a filter, ordered by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, filter: MissionFilter) -> Result<Vec<MissionRow>, Error> {
        self.db
            .read(move |c| {
                let (where_sql, binds) = filter.clauses();
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM missions WHERE {where_sql} ORDER BY name COLLATE NOCASE"
                ))?;

                statement
                    .query_map(params_from_iter(binds), MissionRow::from_row)?
                    .collect()
            })
            .await
    }

    /// One mission by name, soft-deleted rows included.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_name(&self, name: &str) -> Result<Option<MissionRow>, Error> {
        self.one(MissionFilter::by_name(name)).await
    }

    /// One mission by guid, soft-deleted rows included.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_guid(&self, guid: Uuid) -> Result<Option<MissionRow>, Error> {
        self.one(MissionFilter::by_guid(guid)).await
    }

    /// One mission by primary key, soft-deleted rows included.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_id(&self, id: i64) -> Result<Option<MissionRow>, Error> {
        self.db
            .read(move |c| {
                c.query_row(
                    &format!("SELECT {COLUMNS} FROM missions WHERE id = ?1"),
                    [id],
                    MissionRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Applies a patch, stamping `last_edited`.
    ///
    /// An empty patch still stamps, because "somebody saved this" is itself the
    /// change a subscriber is told about.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn update(&self, id: i64, patch: MissionPatch) -> Result<Option<MissionRow>, Error> {
        self.db
            .write(move |tx| {
                let (mut sets, mut binds) = patch_clauses(&patch)?;
                let now = Timestamp::now();

                binds.push(Value::Text(now.to_text()));
                sets.push(format!("last_edited = ?{}", binds.len()));
                binds.push(Value::Text(now.to_text()));
                sets.push(format!("updated_at = ?{}", binds.len()));
                binds.push(Value::Integer(id));

                tx.query_row(
                    &format!(
                        "UPDATE missions SET {} WHERE id = ?{} RETURNING {COLUMNS}",
                        sets.join(", "),
                        binds.len()
                    ),
                    params_from_iter(binds),
                    MissionRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Marks a mission deleted without removing it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn soft_delete(
        &self,
        id: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<MissionRow>, Error> {
        self.db
            .write(move |tx| {
                tx.query_row(
                    &format!(
                        "UPDATE missions SET deleted_at = ?2, updated_at = ?2 \
                         WHERE id = ?1 AND deleted_at IS NULL RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![id, Timestamp::from(at)],
                    MissionRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// The single row a filter matches, if any.
    async fn one(&self, filter: MissionFilter) -> Result<Option<MissionRow>, Error> {
        Ok(self.list(filter).await?.into_iter().next())
    }
}

/// The `SET` clauses and binds a patch turns into.
fn patch_clauses(patch: &MissionPatch) -> rusqlite::Result<(Vec<String>, Vec<Value>)> {
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Value> = Vec::new();

    fn text(column: &str, value: Option<String>, sets: &mut Vec<String>, binds: &mut Vec<Value>) {
        binds.push(value.map_or(Value::Null, Value::Text));
        sets.push(format!("{column} = ?{}", binds.len()));
    }

    if let Some(description) = patch.description.clone() {
        text("description", Some(description), &mut sets, &mut binds);
    }

    for (column, value) in [
        ("chat_room", patch.chat_room.clone()),
        ("base_layer", patch.base_layer.clone()),
        ("bbox", patch.bbox.clone()),
        ("path", patch.path.clone()),
        ("classification", patch.classification.clone()),
        ("password_hash", patch.password_hash.clone()),
    ] {
        if let Some(value) = value {
            text(column, value, &mut sets, &mut binds);
        }
    }

    for (column, value) in [
        ("tool", patch.tool.clone()),
        ("default_role", patch.default_role.clone()),
    ] {
        if let Some(value) = value {
            text(column, Some(value), &mut sets, &mut binds);
        }
    }

    for (column, value) in [
        ("bounding_polygon", patch.bounding_polygon.as_ref()),
        ("keywords", patch.keywords.as_ref()),
        ("groups", patch.groups.as_ref()),
    ] {
        if let Some(value) = value {
            binds.push(Value::Text(to_json(value)?));
            sets.push(format!("{column} = ?{}", binds.len()));
        }
    }

    if let Some(invite_only) = patch.invite_only {
        binds.push(Value::Integer(i64::from(invite_only)));
        sets.push(format!("invite_only = ?{}", binds.len()));
    }

    for (column, value) in [
        ("expiration", patch.expiration),
        ("parent_id", patch.parent_id),
    ] {
        if let Some(value) = value {
            binds.push(value.map_or(Value::Null, Value::Integer));
            sets.push(format!("{column} = ?{}", binds.len()));
        }
    }

    Ok((sets, binds))
}

impl Database {
    /// Data Sync missions.
    pub fn missions(&self) -> MissionsRepo<'_> {
        MissionsRepo::new(self)
    }

    /// A mission's append-only change log.
    pub fn mission_changes(&self) -> changes::MissionChangesRepo<'_> {
        changes::MissionChangesRepo::new(self)
    }

    /// The uids and resources filed under a mission.
    pub fn mission_contents(&self) -> contents::MissionContentsRepo<'_> {
        contents::MissionContentsRepo::new(self)
    }

    /// Who is subscribed to a mission, and with which role.
    pub fn mission_subscriptions(&self) -> subscriptions::MissionSubscriptionsRepo<'_> {
        subscriptions::MissionSubscriptionsRepo::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn sample() -> NewMission {
        NewMission {
            description: "first".to_string(),
            groups: vec!["__ANON__".to_string()],
            keywords: vec!["alpha".to_string()],
            creator_uid: Some("ANDROID-1".to_string()),
            ..NewMission::new("Alpha", "MISSION_SUBSCRIBER")
        }
    }

    #[tokio::test]
    async fn a_mission_round_trips_through_its_json_columns() {
        let db = db().await;
        let created = db.missions().create(sample()).await.unwrap();

        assert_eq!(created.name, "Alpha");
        assert_eq!(created.keywords, vec!["alpha".to_string()]);
        assert_eq!(created.groups, vec!["__ANON__".to_string()]);
        assert_eq!(created.default_role, "MISSION_SUBSCRIBER");
        assert!(created.last_edited.is_none());

        let read = db.missions().by_guid(created.guid).await.unwrap().unwrap();

        assert_eq!(read, created);
    }

    #[tokio::test]
    async fn a_patch_only_touches_what_it_names() {
        let db = db().await;
        let created = db.missions().create(sample()).await.unwrap();

        let updated = db
            .missions()
            .update(
                created.id,
                MissionPatch {
                    description: Some("second".to_string()),
                    ..MissionPatch::default()
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(updated.description, "second");
        assert_eq!(updated.keywords, created.keywords);
        assert_eq!(updated.creator_uid, created.creator_uid);
        assert!(updated.last_edited.is_some());
    }

    #[tokio::test]
    async fn clearing_a_password_is_distinct_from_leaving_it_alone() {
        let db = db().await;
        let created = db
            .missions()
            .create(NewMission {
                password_hash: Some("$argon2id$fake".to_string()),
                ..sample()
            })
            .await
            .unwrap();

        let untouched = db
            .missions()
            .update(created.id, MissionPatch::default())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(untouched.password_hash.as_deref(), Some("$argon2id$fake"));

        let cleared = db
            .missions()
            .update(
                created.id,
                MissionPatch {
                    password_hash: Some(None),
                    ..MissionPatch::default()
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(cleared.password_hash, None);
    }

    #[tokio::test]
    async fn a_soft_delete_leaves_the_row_resolvable_and_out_of_listings() {
        let db = db().await;
        let created = db.missions().create(sample()).await.unwrap();

        db.missions()
            .soft_delete(created.id, Utc::now())
            .await
            .unwrap()
            .unwrap();

        assert!(
            db.missions()
                .list(MissionFilter::tool("public"))
                .await
                .unwrap()
                .is_empty()
        );

        let resolved = db.missions().by_name("Alpha").await.unwrap().unwrap();

        assert!(resolved.is_deleted());
    }

    #[tokio::test]
    async fn the_extras_migration_created_its_tables() {
        let db = db().await;

        let tables: i64 = db
            .read(|c| {
                c.query_one(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
                     AND name IN ('mission_external_data', 'map_layers', 'mission_feeds')",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();

        assert_eq!(tables, 3, "0010 added the three side aggregates");

        let has_token: i64 = db
            .read(|c| {
                c.query_one(
                    "SELECT COUNT(*) FROM pragma_table_info('mission_invitations') \
                     WHERE name = 'token'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();

        assert_eq!(has_token, 1, "invitations carry the whole JWT");
    }
}
