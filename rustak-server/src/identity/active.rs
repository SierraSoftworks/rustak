//! Which channels are currently *switched on*, and for whom.
//!
//! The other half of [`members`](super::members). A membership is a **right**,
//! granted by an administrator or mapped from a `groups` claim; whether a
//! channel is currently switched on is a **preference**, set by a client
//! through `PUT /Marti/api/groups/active`. A subscription's effective rights
//! are the two intersected, which is what [`effective_for_device`] returns.
//!
//! # Three layers, most specific first
//!
//! 1. **The device** (`device_group_state`), so that switching a channel off on
//!    a phone does not switch it off on a laptop.
//! 2. **The account** (`user_group_state`), written by a `PUT` that named no
//!    `clientUid` — which is every caller that is not an end-user device:
//!    CloudTAK's browser half, the admin UI, a script. It is also the default a
//!    device inherits before it has said anything itself, which is what makes a
//!    device enrolled *after* an account-level change route correctly rather
//!    than permissively.
//! 3. **On**, because a client that has never called the endpoint expects
//!    everything it is entitled to.
//!
//! The first two layers are read in the same order by the SQL
//! ([`MembersRepo::effective`]), by this module's default-channel fallback, and
//! by `marti::channels::Selection`. They must not drift apart: the endpoint
//! telling a client one thing while the router does another is precisely the
//! bug the account-level table exists to close.
//!
//! # The default channel
//!
//! Every TAK client expects to be able to talk on `__ANON__` the moment it
//! connects, which is why `[auth] anon_group_default` exists and is on. While
//! it is on, [`effective_for_device`] adds the channel to a subscription that
//! somehow lacks the grant — but a *preference* still overrules it, because
//! somebody who switched the default channel off has switched it off.
//!
//! [`MembersRepo::effective`]: crate::db::repos::MembersRepo::effective

use rustak_api::{ActiveGroup, Direction, GroupName};
use rustak_core::identity::GroupSet;
use rustak_core::prelude::*;

use crate::db::Database;

/// A subscription's effective rights: what the person holds, minus what this
/// device — or, failing that, the account — has switched off.
///
/// A channel neither has said anything about counts as on, because a client
/// that has never called the groups endpoint expects everything it is entitled
/// to. The account layer is `user_group_state`, written by a
/// `PUT /Marti/api/groups/active` that named no `clientUid`, and it is what a
/// device enrolled after such a change inherits.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read fails.
pub async fn effective_for_device(
    db: &Database,
    user_id: UserId,
    device_id: DeviceId,
    anon_by_default: bool,
) -> Result<GroupSet, Error> {
    let mut effective = db.members().effective(user_id, device_id).await?;

    if anon_by_default {
        add_default_channel(db, user_id, Some(device_id), &mut effective).await?;
    }

    Ok(effective)
}

/// What a connection with **no device** may reach.
///
/// The same intersection as [`effective_for_device`] with only the account
/// layer: CloudTAK's browser half, a sidecar and a script all connect without
/// one, and the selection they set through `PUT /Marti/api/groups/active` is
/// the one that should apply to them.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read fails.
pub async fn effective_for_account(
    db: &Database,
    user_id: UserId,
    anon_by_default: bool,
) -> Result<GroupSet, Error> {
    let mut effective = db.user_state().effective(user_id).await?;

    if anon_by_default {
        add_default_channel(db, user_id, None, &mut effective).await?;
    }

    Ok(effective)
}

/// Records which channels a device currently has switched on.
///
/// A state naming a channel that does not exist is dropped rather than
/// refusing the whole call: ATAK sends back the list it was given, and a
/// channel deleted between the two would otherwise break every client that
/// still had it cached.
///
/// Returns how many states were applied, so a caller can tell a request that
/// did nothing from one that did.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("identity.members.set_active", skip_all, fields(device = %device_id), err(Display))]
pub async fn set_active(
    db: &Database,
    device_id: DeviceId,
    states: &[ActiveGroup],
) -> Result<usize, Error> {
    let mut applied = 0;

    for (group_id, direction, active) in resolve_states(db, states).await? {
        db.members()
            .set_active(device_id, group_id, direction, active)
            .await?;

        applied += 1;
    }

    Ok(applied)
}

/// Records which channels an **account** currently has switched on.
///
/// The default every one of its devices inherits until that device says
/// something itself, and the whole answer for a connection that has no device.
/// Unknown channels are dropped for the same reason as [`set_active`].
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("identity.members.set_active_for_user", skip_all, fields(user = %user_id), err(Display))]
pub async fn set_active_for_user(
    db: &Database,
    user_id: UserId,
    states: &[ActiveGroup],
) -> Result<usize, Error> {
    let mut applied = 0;

    for (group_id, direction, active) in resolve_states(db, states).await? {
        db.user_state()
            .set_active(user_id, group_id, direction, active)
            .await?;

        applied += 1;
    }

    Ok(applied)
}

/// Turns the channel names a client sent into the rows a state table takes.
///
/// A state naming a channel that does not exist is dropped rather than
/// refusing the whole call: ATAK sends back the list it was given, and a
/// channel deleted between the two would otherwise break every client that
/// still had it cached.
async fn resolve_states(
    db: &Database,
    states: &[ActiveGroup],
) -> Result<Vec<(GroupId, Direction, bool)>, Error> {
    let groups = db.groups().list().await?;

    Ok(states
        .iter()
        .flat_map(ActiveGroup::expand)
        .filter_map(|state| {
            let Some(group) = groups.iter().find(|group| group.name == state.group) else {
                debug!(group = %state.group, "Ignoring an active-channel state for a channel that is not here.");
                return None;
            };

            Some((group.id, state.direction, state.active))
        })
        .collect())
}

/// Every channel a device has an opinion about, named, for the API.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read fails.
pub async fn active_for_device(
    db: &Database,
    device_id: DeviceId,
) -> Result<Vec<ActiveGroup>, Error> {
    let states = db.members().active_for_device(device_id).await?;

    named_states(
        db,
        states
            .iter()
            .map(|state| (state.group_id, state.direction, state.active)),
    )
    .await
}

/// Every channel an account has an opinion about, named, for the API.
///
/// The default its devices inherit, and what a caller with no device reads
/// back. See [`set_active_for_user`].
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read fails.
pub async fn active_for_user(db: &Database, user_id: UserId) -> Result<Vec<ActiveGroup>, Error> {
    let states = db.user_state().list(user_id).await?;

    named_states(
        db,
        states
            .iter()
            .map(|state| (state.group_id, state.direction, state.active)),
    )
    .await
}

/// Puts channel names on a set of stored states, sorted the way the API reads.
///
/// A state whose channel has since been deleted is dropped rather than named:
/// there is nothing sensible to call it, and a client that still has it cached
/// simply stops being told about it.
async fn named_states(
    db: &Database,
    states: impl Iterator<Item = (GroupId, Direction, bool)>,
) -> Result<Vec<ActiveGroup>, Error> {
    let states: Vec<_> = states.collect();

    if states.is_empty() {
        return Ok(Vec::new());
    }

    let groups = db.groups().list().await?;
    let mut active: Vec<ActiveGroup> = states
        .iter()
        .filter_map(|(group_id, direction, active)| {
            groups
                .iter()
                .find(|group| group.id == *group_id)
                .map(|group| ActiveGroup {
                    group: group.name.clone(),
                    direction: *direction,
                    active: *active,
                })
        })
        .collect();

    active.sort_by(|left, right| {
        (left.group.as_str(), left.direction.as_str())
            .cmp(&(right.group.as_str(), right.direction.as_str()))
    });

    Ok(active)
}

/// Adds `__ANON__` to a subscription that the installation says should have it.
///
/// The device's own preference still applies: somebody who switched the default
/// channel off on their phone has switched it off, whatever the default says.
async fn add_default_channel(
    db: &Database,
    user_id: UserId,
    device_id: Option<DeviceId>,
    effective: &mut GroupSet,
) -> Result<(), Error> {
    let Some(anon) = db.groups().get_by_name(&GroupName::anon()).await? else {
        warn!("The default channel is missing, so nothing was added to a subscription.");
        return Ok(());
    };

    let device = match device_id {
        Some(device_id) => db.members().active_for_device(device_id).await?,
        None => Vec::new(),
    };
    let account = db.user_state().list(user_id).await?;

    for direction in Direction::Both.expand() {
        // The same most-specific-first order the rest of the selection reads
        // in: the device's own answer, then the account's, then on.
        let switched_off = device
            .iter()
            .find(|state| state.group_id == anon.id && state.direction == *direction)
            .map(|state| !state.active)
            .or_else(|| {
                account
                    .iter()
                    .find(|state| state.group_id == anon.id && state.direction == *direction)
                    .map(|state| !state.active)
            })
            .unwrap_or(false);

        if !switched_off {
            effective.set(anon.bitpos, *direction);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::{NewDevice, NewGroup, NewUser};

    struct Fixture {
        db: Database,
        user: UserId,
        device: DeviceId,
        anon: crate::db::repos::GroupRow,
        blue: crate::db::repos::GroupRow,
    }

    async fn fixture() -> Fixture {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("alice").unwrap()))
            .await
            .unwrap()
            .id;
        let device = db
            .devices()
            .create(NewDevice::new(
                DeviceUid::parse("ANDROID-ALICE").unwrap(),
                user,
            ))
            .await
            .unwrap()
            .id;
        let anon = db
            .groups()
            .get_by_name(&GroupName::anon())
            .await
            .unwrap()
            .expect("the default channel is seeded");
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
        db.members()
            .grant(
                user,
                anon.id,
                Direction::Both,
                rustak_api::MembershipSource::Manual,
            )
            .await
            .unwrap();

        Fixture {
            db,
            user,
            device,
            anon,
            blue,
        }
    }

    fn state(name: &str, direction: Direction, active: bool) -> ActiveGroup {
        ActiveGroup {
            group: GroupName::parse(name).unwrap(),
            direction,
            active,
        }
    }

    #[tokio::test]
    async fn a_device_enrolled_after_an_account_level_change_inherits_it() {
        // The gap the `user_group_state` table exists to close. The account
        // switched Blue off through a browser; this device has no state rows of
        // its own, and before the table it would have been routed Blue anyway
        // until it called the endpoint itself.
        let f = fixture().await;

        set_active_for_user(&f.db, f.user, &[state("Blue", Direction::Both, false)])
            .await
            .unwrap();

        let effective = effective_for_device(&f.db, f.user, f.device, true)
            .await
            .unwrap();

        assert!(!effective.contains(f.blue.bitpos, Direction::Out));
        assert!(!effective.contains(f.blue.bitpos, Direction::In));
    }

    #[tokio::test]
    async fn the_device_overrules_the_account() {
        let f = fixture().await;

        set_active_for_user(&f.db, f.user, &[state("Blue", Direction::Both, false)])
            .await
            .unwrap();
        set_active(&f.db, f.device, &[state("Blue", Direction::Out, true)])
            .await
            .unwrap();

        let effective = effective_for_device(&f.db, f.user, f.device, true)
            .await
            .unwrap();

        assert!(effective.contains(f.blue.bitpos, Direction::Out));
        assert!(
            !effective.contains(f.blue.bitpos, Direction::In),
            "the direction the device said nothing about still follows the account",
        );
    }

    #[tokio::test]
    async fn an_account_that_switched_the_default_channel_off_does_not_get_it_back() {
        // `anon_group_default` re-adds the grant, never the preference.
        let f = fixture().await;

        set_active_for_user(&f.db, f.user, &[state("__ANON__", Direction::Both, false)])
            .await
            .unwrap();

        let effective = effective_for_device(&f.db, f.user, f.device, true)
            .await
            .unwrap();

        assert!(!effective.contains(f.anon.bitpos, Direction::Out));
    }

    #[tokio::test]
    async fn a_connection_with_no_device_reads_the_accounts_selection() {
        // CloudTAK's browser half, a sidecar, a script: no device row to narrow
        // by, so the account's own selection is the whole answer.
        let f = fixture().await;

        set_active_for_user(&f.db, f.user, &[state("Blue", Direction::Out, false)])
            .await
            .unwrap();

        let effective = effective_for_account(&f.db, f.user, true).await.unwrap();

        assert!(!effective.contains(f.blue.bitpos, Direction::Out));
        assert!(effective.contains(f.blue.bitpos, Direction::In));
        assert!(effective.contains(f.anon.bitpos, Direction::Out));
    }

    #[tokio::test]
    async fn a_state_for_a_channel_that_is_gone_is_dropped_rather_than_refused() {
        let f = fixture().await;

        let applied = set_active_for_user(
            &f.db,
            f.user,
            &[
                state("Vanished", Direction::Both, false),
                state("Blue", Direction::In, false),
            ],
        )
        .await
        .unwrap();

        assert_eq!(applied, 1);
        assert_eq!(active_for_user(&f.db, f.user).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_named_states_come_back_sorted_for_the_api() {
        let f = fixture().await;

        set_active_for_user(
            &f.db,
            f.user,
            &[
                state("Blue", Direction::Both, false),
                state("__ANON__", Direction::In, true),
            ],
        )
        .await
        .unwrap();

        let named: Vec<(String, &str)> = active_for_user(&f.db, f.user)
            .await
            .unwrap()
            .iter()
            .map(|state| (state.group.to_string(), state.direction.as_str()))
            .collect();

        assert_eq!(
            named,
            vec![
                ("Blue".to_string(), "IN"),
                ("Blue".to_string(), "OUT"),
                ("__ANON__".to_string(), "IN"),
            ],
        );
    }
}
