//! `groups`: channels, and the bit positions routing runs on.
//!
//! Every channel owns a bit in the 256-wide [`GroupSet`] the hub intersects to
//! decide who may see what, so a bit position is allocated once and freed only
//! deliberately. Deleting a channel soft-deletes the row and keeps its bit
//! reserved until every live subscription has been refreshed; reusing it sooner
//! would hand one channel's traffic to the members of another.
//!
//! [`GroupSet`]: rustak_core::identity::GroupSet

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_api::GroupSource;
use rustak_core::{identity::GROUP_BITS, prelude::*};

use crate::db::{
    Database,
    row::{enum_col, id_col, opt_ts, ts},
};

/// The columns [`GroupRow::from_row`] expects, in order.
const COLUMNS: &str = "id, name, bitpos, description, source, created_at, deleted_at";

/// One row of `groups`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    pub id: GroupId,
    pub name: GroupName,
    /// Which bit of a [`GroupSet`] this channel occupies.
    pub bitpos: u32,
    pub description: Option<String>,
    pub source: GroupSource,
    pub created_at: DateTime<Utc>,
    /// Set when the channel was deleted; its bit stays reserved until the row
    /// is purged.
    pub deleted_at: Option<DateTime<Utc>>,
}

impl GroupRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            name: GroupName::from_storage(row.get::<_, String>(1)?),
            bitpos: row.get::<_, i64>(2)?.max(0) as u32,
            description: row.get(3)?,
            source: enum_col(row, 4, GroupSource::parse)?,
            created_at: ts(row, 5)?,
            deleted_at: opt_ts(row, 6)?,
        })
    }
}

/// A channel about to be created.
#[derive(Debug, Clone)]
pub struct NewGroup {
    pub name: GroupName,
    pub description: Option<String>,
    pub source: GroupSource,
}

impl NewGroup {
    /// A channel an administrator asked for.
    pub fn manual(name: GroupName) -> Self {
        Self {
            name,
            description: None,
            source: GroupSource::Manual,
        }
    }

    /// A channel created because a `groups` claim named one.
    pub fn from_claims(name: GroupName) -> Self {
        Self {
            source: GroupSource::Oidc,
            ..Self::manual(name)
        }
    }
}

/// Reads and writes `groups`.
pub struct GroupsRepo<'a> {
    db: &'a Database,
}

impl<'a> GroupsRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Creates a channel, allocating the lowest free bit position.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when every bit is taken, because
    /// the operator has to delete a channel before adding another; a
    /// [`human_errors::Kind::System`] error for a name clash or a failed write.
    pub async fn create(&self, new: NewGroup) -> Result<GroupRow, Error> {
        let created = self
            .db
            .write(move |tx| {
                // Allocated inside the write transaction, which holds the write
                // lock: two administrators adding a channel at once cannot be
                // handed the same bit.
                let Some(bitpos) = next_free_bitpos(tx)? else {
                    return Ok(None);
                };

                tx.query_one(
                    &format!(
                        "INSERT INTO groups (name, bitpos, description, source, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.name.as_str(),
                        bitpos,
                        new.description,
                        new.source.as_str(),
                        crate::db::row::Timestamp::now(),
                    ],
                    GroupRow::from_row,
                )
                .map(Some)
            })
            .await?;

        created.ok_or_else(|| {
            human_errors::user(
                format!("There is no room for another channel: all {GROUP_BITS} are in use."),
                &[
                    "Delete a channel that is no longer used, then create this one.",
                    "A deleted channel's position is reclaimed once its subscriptions have been refreshed.",
                ],
            )
        })
    }

    /// Reads one channel by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: GroupId) -> Result<Option<GroupRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM groups WHERE id = ?1"),
                    [id.get()],
                    GroupRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads one channel by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_name(&self, name: &GroupName) -> Result<Option<GroupRow>, Error> {
        let name = name.as_str().to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM groups WHERE name = ?1"),
                    [name],
                    GroupRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Every live channel, by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self) -> Result<Vec<GroupRow>, Error> {
        self.list_where(false).await
    }

    /// Every channel including the deleted ones, whose bits are still reserved.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_all(&self) -> Result<Vec<GroupRow>, Error> {
        self.list_where(true).await
    }

    async fn list_where(&self, include_deleted: bool) -> Result<Vec<GroupRow>, Error> {
        self.db
            .read(move |c| {
                let filter = if include_deleted {
                    ""
                } else {
                    "WHERE deleted_at IS NULL"
                };
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM groups {filter} ORDER BY bitpos ASC"
                ))?;

                statement.query_map([], GroupRow::from_row)?.collect()
            })
            .await
    }

    /// Builds the name-to-bit index the router keeps in memory.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn index(&self) -> Result<rustak_core::identity::GroupIndex, Error> {
        let mut index = rustak_core::identity::GroupIndex::new();

        for group in self.list().await? {
            index.insert(group.bitpos, group.name);
        }

        Ok(index)
    }

    /// Changes a channel's description.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_description(
        &self,
        id: GroupId,
        description: Option<String>,
    ) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE groups SET description = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), description],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Soft-deletes a channel, keeping its bit reserved.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when asked to delete `__ANON__`,
    /// which every principal is a member of by default and which nothing would
    /// work without.
    pub async fn soft_delete(&self, id: GroupId) -> Result<bool, Error> {
        let anon = self
            .get(id)
            .await?
            .is_some_and(|group| group.source == GroupSource::System);

        if anon {
            return Err(human_errors::user(
                "The default channel cannot be deleted.",
                &["Remove the members from it instead, or make another channel the default."],
            ));
        }

        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE groups SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                    rusqlite::params![id.get(), crate::db::row::Timestamp::now()],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Removes a soft-deleted channel and frees its bit for reuse.
    ///
    /// Only safe once every live subscription has been refreshed, which is why
    /// it is a separate step rather than part of [`GroupsRepo::soft_delete`].
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn purge_deleted(&self, id: GroupId) -> Result<bool, Error> {
        let purged = self
            .db
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM groups WHERE id = ?1 AND deleted_at IS NOT NULL",
                    [id.get()],
                )
            })
            .await?;

        Ok(purged > 0)
    }
}

/// The lowest bit position not already spoken for, or `None` when there is
/// none left.
///
/// Bit 0 is never handed out and bit 1 belongs to `__ANON__`, seeded by the
/// migration, so allocation starts above whatever is already there.
fn next_free_bitpos(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<Option<i64>> {
    let taken: Vec<i64> = {
        let mut statement = tx.prepare("SELECT bitpos FROM groups ORDER BY bitpos ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;

        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut candidate = 1;
    for bit in taken {
        if bit == candidate {
            candidate += 1;
        }
    }

    if candidate >= GROUP_BITS as i64 {
        return Ok(None);
    }

    Ok(Some(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn channel(raw: &str) -> GroupName {
        GroupName::parse(raw).unwrap()
    }

    #[tokio::test]
    async fn the_default_channel_is_seeded_at_the_bit_the_router_expects() {
        let db = db().await;

        let anon = db
            .groups()
            .get_by_name(&GroupName::anon())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(anon.bitpos, rustak_core::identity::ANON_BITPOS);
        assert_eq!(anon.source, GroupSource::System);
    }

    #[tokio::test]
    async fn new_channels_take_the_lowest_free_bit() {
        let db = db().await;

        let blue = db
            .groups()
            .create(NewGroup::manual(channel("Blue")))
            .await
            .unwrap();
        let red = db
            .groups()
            .create(NewGroup::manual(channel("Red")))
            .await
            .unwrap();

        assert_eq!(blue.bitpos, 2);
        assert_eq!(red.bitpos, 3);
    }

    #[tokio::test]
    async fn a_purged_channels_bit_is_reused_and_a_deleted_ones_is_not() {
        let db = db().await;
        let blue = db
            .groups()
            .create(NewGroup::manual(channel("Blue")))
            .await
            .unwrap();

        assert!(db.groups().soft_delete(blue.id).await.unwrap());
        let red = db
            .groups()
            .create(NewGroup::manual(channel("Red")))
            .await
            .unwrap();
        assert_ne!(
            red.bitpos, blue.bitpos,
            "a live subscription may still hold it"
        );

        assert!(db.groups().purge_deleted(blue.id).await.unwrap());
        let green = db
            .groups()
            .create(NewGroup::manual(channel("Green")))
            .await
            .unwrap();
        assert_eq!(green.bitpos, blue.bitpos);
    }

    #[tokio::test]
    async fn channel_names_are_unique() {
        let db = db().await;
        db.groups()
            .create(NewGroup::manual(channel("Blue")))
            .await
            .unwrap();

        assert!(
            db.groups()
                .create(NewGroup::manual(channel("Blue")))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_default_channel_cannot_be_deleted() {
        let db = db().await;
        let anon = db
            .groups()
            .get_by_name(&GroupName::anon())
            .await
            .unwrap()
            .unwrap();

        assert!(db.groups().soft_delete(anon.id).await.is_err());
    }

    #[tokio::test]
    async fn listing_hides_deleted_channels_but_list_all_shows_them() {
        let db = db().await;
        let blue = db
            .groups()
            .create(NewGroup::manual(channel("Blue")))
            .await
            .unwrap();
        db.groups().soft_delete(blue.id).await.unwrap();

        assert_eq!(db.groups().list().await.unwrap().len(), 1);
        assert_eq!(db.groups().list_all().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_index_maps_bits_back_to_names() {
        let db = db().await;
        let blue = db
            .groups()
            .create(NewGroup::manual(channel("Blue")))
            .await
            .unwrap();

        let index = db.groups().index().await.unwrap();

        assert_eq!(index.name(blue.bitpos), Some(&blue.name));
        assert_eq!(index.bitpos(&GroupName::anon()), Some(1));
    }

    #[tokio::test]
    async fn a_description_can_be_set_and_cleared() {
        let db = db().await;
        let blue = db
            .groups()
            .create(NewGroup::manual(channel("Blue")))
            .await
            .unwrap();

        assert!(
            db.groups()
                .set_description(blue.id, Some("Command net".into()))
                .await
                .unwrap()
        );
        assert_eq!(
            db.groups().get(blue.id).await.unwrap().unwrap().description,
            Some("Command net".into())
        );

        db.groups().set_description(blue.id, None).await.unwrap();
        assert_eq!(
            db.groups().get(blue.id).await.unwrap().unwrap().description,
            None
        );
    }
}
