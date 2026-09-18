//! `user_group_state`: which channels an *account* has switched on.
//!
//! The sibling of [`device_group_state`](super::members), and the two are read
//! most-specific-first. A membership is a right; whether a channel is currently
//! switched on is a preference, and rustak scopes that preference to a device
//! so that switching a channel off on a phone does not switch it off on a
//! laptop. But `PUT /Marti/api/groups/active` arrives with no `clientUid` from
//! every caller that is not an end-user device — CloudTAK's browser half, the
//! admin UI, a script — and those callers have no device row to write to.
//!
//! That selection used to live in the key/value store, where the Marti reads
//! could see it and the routing path could not. It is a table now for exactly
//! one reason: [`MembersRepo::effective`](super::MembersRepo::effective) can
//! join it, so a device enrolled *after* an account-level change inherits the
//! account's answer instead of routing permissively until it calls the endpoint
//! itself.

use rustak_core::{identity::GroupSet, prelude::*};

use crate::db::{
    Database,
    row::{bool_col, enum_col, id_col},
};

use super::members::to_set;

/// One row of `user_group_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountChannel {
    pub user_id: UserId,
    pub group_id: GroupId,
    /// Always `In` or `Out`; never `Both`.
    pub direction: Direction,
    pub active: bool,
}

/// Reads and writes the account-level active state.
pub struct UserStateRepo<'a> {
    db: &'a Database,
}

impl<'a> UserStateRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Records whether an account currently has a channel switched on.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails, including
    /// when the account or the channel does not exist.
    pub async fn set_active(
        &self,
        user_id: UserId,
        group_id: GroupId,
        direction: Direction,
        active: bool,
    ) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                for single in direction.expand() {
                    tx.execute(
                        "INSERT INTO user_group_state (user_id, group_id, direction, active) \
                         VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT (user_id, group_id, direction) \
                         DO UPDATE SET active = excluded.active",
                        rusqlite::params![
                            user_id.get(),
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

    /// Every channel an account has an opinion about.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, user_id: UserId) -> Result<Vec<AccountChannel>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT user_id, group_id, direction, active FROM user_group_state \
                     WHERE user_id = ?1 ORDER BY group_id ASC, direction ASC",
                )?;
                let rows = statement.query_map([user_id.get()], |row| {
                    Ok(AccountChannel {
                        user_id: id_col(row, 0)?,
                        group_id: id_col(row, 1)?,
                        direction: enum_col(row, 2, Direction::parse)?,
                        active: bool_col(row, 3)?,
                    })
                })?;

                rows.collect()
            })
            .await
    }

    /// What a connection with no device may reach: the account's grants, minus
    /// anything the account itself has switched off.
    ///
    /// The no-device counterpart of
    /// [`MembersRepo::effective`](super::MembersRepo::effective), and the reason
    /// `group_set` is left alone: that one answers *entitlement*, which the
    /// admin API and the membership listings ask about, and a channel somebody
    /// has switched off is still a channel they hold.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn effective(&self, user_id: UserId) -> Result<GroupSet, Error> {
        let bits = self
            .db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT g.bitpos, m.direction FROM group_members m \
                     JOIN groups g ON g.id = m.group_id \
                     LEFT JOIN user_group_state u \
                       ON u.group_id = m.group_id AND u.direction = m.direction \
                       AND u.user_id = m.user_id \
                     WHERE m.user_id = ?1 AND g.deleted_at IS NULL \
                       AND COALESCE(u.active, 1) = 1",
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::{NewGroup, NewUser};

    struct Fixture {
        db: Database,
        user: UserId,
        blue: GroupId,
        blue_bit: u32,
    }

    async fn fixture() -> Fixture {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("j.smith").unwrap()))
            .await
            .unwrap()
            .id;
        let blue = db
            .groups()
            .create(NewGroup::manual(GroupName::parse("Blue").unwrap()))
            .await
            .unwrap();

        db.members()
            .grant(
                user,
                blue.id,
                Direction::Both,
                rustak_api::MembershipSource::Manual,
            )
            .await
            .unwrap();

        Fixture {
            db,
            user,
            blue: blue.id,
            blue_bit: blue.bitpos,
        }
    }

    #[tokio::test]
    async fn an_account_with_no_opinion_reaches_everything_it_holds() {
        let f = fixture().await;

        let set = f.db.user_state().effective(f.user).await.unwrap();

        assert!(set.contains(f.blue_bit, Direction::In));
        assert!(set.contains(f.blue_bit, Direction::Out));
        assert!(f.db.user_state().list(f.user).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_channel_the_account_switched_off_leaves_the_effective_set() {
        let f = fixture().await;

        f.db.user_state()
            .set_active(f.user, f.blue, Direction::Out, false)
            .await
            .unwrap();

        let set = f.db.user_state().effective(f.user).await.unwrap();

        assert!(!set.contains(f.blue_bit, Direction::Out));
        assert!(
            set.contains(f.blue_bit, Direction::In),
            "the direction nobody switched off is untouched",
        );
    }

    #[tokio::test]
    async fn both_expands_to_one_row_per_direction_and_the_last_write_wins() {
        let f = fixture().await;

        f.db.user_state()
            .set_active(f.user, f.blue, Direction::Both, false)
            .await
            .unwrap();

        assert_eq!(f.db.user_state().list(f.user).await.unwrap().len(), 2);

        f.db.user_state()
            .set_active(f.user, f.blue, Direction::In, true)
            .await
            .unwrap();

        let rows = f.db.user_state().list(f.user).await.unwrap();

        assert_eq!(rows.len(), 2, "an update, not a second row");
        assert!(
            rows.iter()
                .any(|row| row.direction == Direction::In && row.active)
        );
        assert!(
            rows.iter()
                .any(|row| row.direction == Direction::Out && !row.active)
        );
    }

    #[tokio::test]
    async fn a_selection_does_not_grant_a_channel_the_account_does_not_hold() {
        // The state is a preference over rights, never a right of its own.
        let f = fixture().await;
        let red =
            f.db.groups()
                .create(NewGroup::manual(GroupName::parse("Red").unwrap()))
                .await
                .unwrap();

        f.db.user_state()
            .set_active(f.user, red.id, Direction::Both, true)
            .await
            .unwrap();

        let set = f.db.user_state().effective(f.user).await.unwrap();

        assert!(!set.contains(red.bitpos, Direction::Out));
    }
}
