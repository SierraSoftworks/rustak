//! `group_members` and `device_group_state`: who may reach which channel, and
//! which of those a given device currently has switched on.
//!
//! The two tables answer different questions and must not be collapsed into
//! one. A membership is a *right*, granted by an administrator or mapped from a
//! `groups` claim; the active state is a *preference*, set by the client
//! through `PUT /Marti/api/groups/active?clientUid=` and scoped to one device,
//! so that switching a channel off on a phone does not switch it off on a
//! laptop. A subscription's effective rights are the two intersected, which is
//! what [`MembersRepo::effective`] computes.
//!
//! Storage holds one row per single direction. `Direction::Both` is a UI
//! convenience and never reaches a column, so everything here expands it first.

use rusqlite::OptionalExtension as _;
use rustak_api::MembershipSource;
use rustak_core::{identity::GroupSet, prelude::*};

use crate::db::{
    Database,
    row::{bool_col, enum_col, id_col},
};

/// One row of `group_members`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    pub user_id: UserId,
    pub group_id: GroupId,
    /// Always `In` or `Out`; never `Both`.
    pub direction: Direction,
    pub source: MembershipSource,
}

/// One row of `device_group_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveChannel {
    pub device_id: DeviceId,
    pub group_id: GroupId,
    /// Always `In` or `Out`; never `Both`.
    pub direction: Direction,
    pub active: bool,
}

/// Reads and writes channel membership and per-device active state.
pub struct MembersRepo<'a> {
    db: &'a Database,
}

impl<'a> MembersRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Grants a user a channel in one or both directions.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the user or the
    /// channel does not exist — the foreign keys refuse a dangling grant.
    pub async fn grant(
        &self,
        user_id: UserId,
        group_id: GroupId,
        direction: Direction,
        source: MembershipSource,
    ) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                for single in direction.expand() {
                    tx.execute(
                        "INSERT INTO group_members (user_id, group_id, direction, source) \
                         VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT (user_id, group_id, direction) \
                         DO UPDATE SET source = excluded.source",
                        rusqlite::params![
                            user_id.get(),
                            group_id.get(),
                            single.as_str(),
                            source.as_str()
                        ],
                    )?;
                }

                Ok(())
            })
            .await
    }

    /// Removes a grant, reporting how many rows went.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke(
        &self,
        user_id: UserId,
        group_id: GroupId,
        direction: Direction,
    ) -> Result<usize, Error> {
        self.db
            .write(move |tx| {
                let mut removed = 0;

                for single in direction.expand() {
                    removed += tx.execute(
                        "DELETE FROM group_members \
                         WHERE user_id = ?1 AND group_id = ?2 AND direction = ?3",
                        rusqlite::params![user_id.get(), group_id.get(), single.as_str()],
                    )?;
                }

                Ok(removed)
            })
            .await
    }

    /// Every grant a user holds.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Membership>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT user_id, group_id, direction, source FROM group_members \
                     WHERE user_id = ?1 ORDER BY group_id ASC, direction ASC",
                )?;

                statement
                    .query_map([user_id.get()], membership_from_row)?
                    .collect()
            })
            .await
    }

    /// Every grant on a channel.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_for_group(&self, group_id: GroupId) -> Result<Vec<Membership>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT user_id, group_id, direction, source FROM group_members \
                     WHERE group_id = ?1 ORDER BY user_id ASC, direction ASC",
                )?;

                statement
                    .query_map([group_id.get()], membership_from_row)?
                    .collect()
            })
            .await
    }

    /// Replaces every grant an identity provider owns with `granted`, leaving
    /// the ones an administrator made alone.
    ///
    /// Run at each sign-in: a claim that stopped being sent has to stop granting
    /// anything, and doing it as one transaction means the user is never
    /// momentarily in no channels at all.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn replace_provider_grants(
        &self,
        user_id: UserId,
        granted: Vec<(GroupId, Direction)>,
    ) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM group_members WHERE user_id = ?1 AND source = 'oidc'",
                    [user_id.get()],
                )?;

                for (group_id, direction) in granted {
                    for single in direction.expand() {
                        tx.execute(
                            "INSERT INTO group_members (user_id, group_id, direction, source) \
                             VALUES (?1, ?2, ?3, 'oidc') \
                             ON CONFLICT (user_id, group_id, direction) DO NOTHING",
                            rusqlite::params![user_id.get(), group_id.get(), single.as_str()],
                        )?;
                    }
                }

                Ok(())
            })
            .await
    }

    /// A user's grants as the bit vector the router routes on.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn group_set(&self, user_id: UserId) -> Result<GroupSet, Error> {
        let bits = self
            .db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT g.bitpos, m.direction FROM group_members m \
                     JOIN groups g ON g.id = m.group_id \
                     WHERE m.user_id = ?1 AND g.deleted_at IS NULL",
                )?;
                let rows = statement.query_map([user_id.get()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u32,
                        enum_col(row, 1, Direction::parse)?,
                    ))
                })?;

                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await?;

        Ok(to_set(bits))
    }

    /// Records whether a device currently has a channel switched on.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_active(
        &self,
        device_id: DeviceId,
        group_id: GroupId,
        direction: Direction,
        active: bool,
    ) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                for single in direction.expand() {
                    tx.execute(
                        "INSERT INTO device_group_state (device_id, group_id, direction, active) \
                         VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT (device_id, group_id, direction) \
                         DO UPDATE SET active = excluded.active",
                        rusqlite::params![
                            device_id.get(),
                            group_id.get(),
                            single.as_str(),
                            i64::from(active)
                        ],
                    )?;
                }

                Ok(())
            })
            .await
    }

    /// Every channel a device has an opinion about.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn active_for_device(
        &self,
        device_id: DeviceId,
    ) -> Result<Vec<ActiveChannel>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT device_id, group_id, direction, active FROM device_group_state \
                     WHERE device_id = ?1 ORDER BY group_id ASC, direction ASC",
                )?;
                let rows = statement.query_map([device_id.get()], |row| {
                    Ok(ActiveChannel {
                        device_id: id_col(row, 0)?,
                        group_id: id_col(row, 1)?,
                        direction: enum_col(row, 2, Direction::parse)?,
                        active: bool_col(row, 3)?,
                    })
                })?;

                rows.collect()
            })
            .await
    }

    /// A subscription's effective rights: the user's grants, minus anything the
    /// device has explicitly switched off.
    ///
    /// A channel the device has said nothing about counts as on, because a
    /// client that has never called the groups endpoint expects everything it
    /// is entitled to.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn effective(&self, user_id: UserId, device_id: DeviceId) -> Result<GroupSet, Error> {
        let bits = self
            .db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT g.bitpos, m.direction FROM group_members m \
                     JOIN groups g ON g.id = m.group_id \
                     LEFT JOIN device_group_state d \
                       ON d.group_id = m.group_id AND d.direction = m.direction \
                       AND d.device_id = ?2 \
                     WHERE m.user_id = ?1 AND g.deleted_at IS NULL \
                       AND COALESCE(d.active, 1) = 1",
                )?;
                let rows = statement.query_map([user_id.get(), device_id.get()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u32,
                        enum_col(row, 1, Direction::parse)?,
                    ))
                })?;

                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await?;

        Ok(to_set(bits))
    }

    /// Whether a user holds a particular grant.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn holds(
        &self,
        user_id: UserId,
        group_id: GroupId,
        direction: Direction,
    ) -> Result<bool, Error> {
        for single in direction.expand() {
            let found: Option<i64> = self
                .db
                .read(move |c| {
                    c.query_one(
                        "SELECT 1 FROM group_members \
                         WHERE user_id = ?1 AND group_id = ?2 AND direction = ?3",
                        rusqlite::params![user_id.get(), group_id.get(), single.as_str()],
                        |row| row.get(0),
                    )
                    .optional()
                })
                .await?;

            if found.is_none() {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

fn membership_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Membership> {
    Ok(Membership {
        user_id: id_col(row, 0)?,
        group_id: id_col(row, 1)?,
        direction: enum_col(row, 2, Direction::parse)?,
        source: enum_col(row, 3, MembershipSource::parse)?,
    })
}

fn to_set(bits: Vec<(u32, Direction)>) -> GroupSet {
    let mut set = GroupSet::new();

    for (bitpos, direction) in bits {
        set.set(bitpos, direction);
    }

    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::{NewDevice, NewGroup, NewUser};

    struct Fixture {
        db: Database,
        user: UserId,
        device: DeviceId,
        blue: GroupId,
        red: GroupId,
        blue_bit: u32,
    }

    async fn fixture() -> Fixture {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("j.smith").unwrap()))
            .await
            .unwrap();
        let device = db
            .devices()
            .create(NewDevice::new(
                DeviceUid::parse("ANDROID-1").unwrap(),
                user.id,
            ))
            .await
            .unwrap();
        let blue = db
            .groups()
            .create(NewGroup::manual(GroupName::parse("Blue").unwrap()))
            .await
            .unwrap();
        let red = db
            .groups()
            .create(NewGroup::manual(GroupName::parse("Red").unwrap()))
            .await
            .unwrap();

        Fixture {
            db,
            user: user.id,
            device: device.id,
            blue: blue.id,
            red: red.id,
            blue_bit: blue.bitpos,
        }
    }

    #[tokio::test]
    async fn a_both_grant_is_stored_as_two_single_direction_rows() {
        let f = fixture().await;

        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();

        let held = f.db.members().list_for_user(f.user).await.unwrap();
        assert_eq!(held.len(), 2);
        assert!(held.iter().all(|m| m.direction != Direction::Both));
        assert!(
            f.db.members()
                .holds(f.user, f.blue, Direction::Both)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn granting_twice_updates_rather_than_failing() {
        let f = fixture().await;

        f.db.members()
            .grant(f.user, f.blue, Direction::In, MembershipSource::Manual)
            .await
            .unwrap();
        f.db.members()
            .grant(f.user, f.blue, Direction::In, MembershipSource::Oidc)
            .await
            .unwrap();

        let held = f.db.members().list_for_user(f.user).await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].source, MembershipSource::Oidc);
    }

    #[tokio::test]
    async fn a_grant_for_a_user_or_channel_that_does_not_exist_is_refused() {
        let f = fixture().await;

        assert!(
            f.db.members()
                .grant(
                    UserId::new(9999),
                    f.blue,
                    Direction::In,
                    MembershipSource::Manual
                )
                .await
                .is_err()
        );
        assert!(
            f.db.members()
                .grant(
                    f.user,
                    GroupId::new(9999),
                    Direction::In,
                    MembershipSource::Manual
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn deleting_a_user_takes_their_memberships_with_them() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();

        f.db.users().delete(f.user).await.unwrap();

        assert!(
            f.db.members()
                .list_for_group(f.blue)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn refreshing_provider_grants_leaves_manual_ones_alone() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();
        f.db.members()
            .grant(f.user, f.red, Direction::Out, MembershipSource::Oidc)
            .await
            .unwrap();

        f.db.members()
            .replace_provider_grants(f.user, vec![])
            .await
            .unwrap();

        let held = f.db.members().list_for_user(f.user).await.unwrap();
        assert_eq!(held.len(), 2);
        assert!(held.iter().all(|m| m.group_id == f.blue));
    }

    #[tokio::test]
    async fn the_group_set_carries_the_channels_bit_positions() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Out, MembershipSource::Manual)
            .await
            .unwrap();

        let set = f.db.members().group_set(f.user).await.unwrap();

        assert!(set.contains(f.blue_bit, Direction::Out));
        assert!(!set.contains(f.blue_bit, Direction::In));
    }

    #[tokio::test]
    async fn a_channel_the_device_has_not_spoken_about_counts_as_on() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();

        let effective = f.db.members().effective(f.user, f.device).await.unwrap();
        assert!(effective.contains(f.blue_bit, Direction::Both));
    }

    #[tokio::test]
    async fn switching_a_channel_off_removes_it_from_the_effective_set_only() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();

        f.db.members()
            .set_active(f.device, f.blue, Direction::Out, false)
            .await
            .unwrap();

        let effective = f.db.members().effective(f.user, f.device).await.unwrap();
        assert!(!effective.contains(f.blue_bit, Direction::Out));
        assert!(
            effective.contains(f.blue_bit, Direction::In),
            "only OUT was switched off"
        );

        // The grant itself is untouched.
        assert!(
            f.db.members()
                .group_set(f.user)
                .await
                .unwrap()
                .contains(f.blue_bit, Direction::Out)
        );
        assert_eq!(
            f.db.members()
                .active_for_device(f.device)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_deleted_channel_grants_nothing() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();

        f.db.groups().soft_delete(f.blue).await.unwrap();

        assert!(f.db.members().group_set(f.user).await.unwrap().is_empty());
        assert!(
            f.db.members()
                .effective(f.user, f.device)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn revoking_reports_how_many_rows_went() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue, Direction::Both, MembershipSource::Manual)
            .await
            .unwrap();

        assert_eq!(
            f.db.members()
                .revoke(f.user, f.blue, Direction::Both)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            f.db.members()
                .revoke(f.user, f.blue, Direction::Both)
                .await
                .unwrap(),
            0
        );
    }
}
