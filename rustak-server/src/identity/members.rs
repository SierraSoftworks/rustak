//! Who may reach which channel: the *rights* half.
//!
//! Two questions that look alike and must not be collapsed. A **membership** is
//! a right, granted by an administrator or mapped from a `groups` claim, and it
//! is what this file reads and writes. The **active state** is a preference —
//! `PUT /Marti/api/groups/active`, per device over per account — and it lives
//! in [`active`](super::active), which this module re-exports so that every
//! existing `members::effective_for_device` caller still names the same
//! function. A subscription's effective rights are the two intersected.
//!
//! # The default channel
//!
//! Every TAK client expects to be able to talk on `__ANON__` the moment it
//! connects, which is why `[auth] anon_group_default` exists and is on. While it
//! is on, the grant is not an administrator's to remove: [`replace_manual`] puts
//! it back, and [`effective_for_device`] adds it to a subscription that somehow
//! lacks it. An installation that turns the setting off is saying it will grant
//! every channel by hand, and then nothing here re-adds anything.

use rustak_api::{Direction, GroupMembership, GroupName, MembershipSource};
use rustak_core::prelude::*;

use crate::db::{
    Database,
    repos::{GroupRow, Membership},
};
use crate::services::{AppContext, Services};

pub use super::active::{
    active_for_device, active_for_user, effective_for_account, effective_for_device, set_active,
    set_active_for_user,
};

/// A person's channels, named and sorted, for the API.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if either read fails.
pub async fn grants_for_user(
    db: &Database,
    user_id: UserId,
) -> Result<Vec<GroupMembership>, Error> {
    let held = db.members().list_for_user(user_id).await?;

    if held.is_empty() {
        return Ok(Vec::new());
    }

    let groups = db.groups().list().await?;
    let mut grants: Vec<GroupMembership> = held
        .iter()
        .filter_map(|membership| {
            groups
                .iter()
                .find(|group| group.id == membership.group_id)
                .map(|group| GroupMembership {
                    group: group.name.clone(),
                    direction: membership.direction,
                    source: Some(membership.source),
                })
        })
        .collect();

    grants.sort_by(|left, right| {
        (left.group.as_str(), left.direction.as_str())
            .cmp(&(right.group.as_str(), right.direction.as_str()))
    });

    Ok(grants)
}

/// Replaces the grants an administrator made, leaving the provider's alone.
///
/// Applied as a delta rather than a delete followed by an insert: a member is
/// never momentarily in no channels at all, which matters because a live
/// subscription re-reads these.
///
/// A grant whose channel does not exist is refused rather than skipped — an
/// administrator who mistypes a channel name should be told, not left looking
/// at a membership list that quietly lost a row.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error naming a channel that does not exist,
/// and a [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("identity.members.replace_manual", skip_all, fields(user = %user_id), err(Display))]
pub async fn replace_manual(
    db: &Database,
    user_id: UserId,
    wanted: &[GroupMembership],
    anon_by_default: bool,
) -> Result<Vec<GroupMembership>, Error> {
    let groups = db.groups().list().await?;
    let mut wanted = resolve(&groups, wanted)?;

    if anon_by_default {
        // The installation says everybody is on the default channel, so an
        // administrator taking it away here would only be overruled the next
        // time anything asked.
        if let Some(anon) = groups.iter().find(|group| group.name.is_anon()) {
            for direction in Direction::Both.expand() {
                if !wanted.contains(&(anon.id, *direction)) {
                    wanted.push((anon.id, *direction));
                }
            }
        }
    }

    let held = db.members().list_for_user(user_id).await?;

    for membership in held.iter().filter(is_manual) {
        if !wanted.contains(&(membership.group_id, membership.direction)) {
            db.members()
                .revoke(user_id, membership.group_id, membership.direction)
                .await?;
        }
    }

    for (group_id, direction) in wanted {
        if !held
            .iter()
            .any(|held| held.group_id == group_id && held.direction == direction)
        {
            db.members()
                .grant(user_id, group_id, direction, MembershipSource::Manual)
                .await?;
        }
    }

    grants_for_user(db, user_id).await
}

/// Makes a channel change take effect on whatever the account has connected.
///
/// Two things happen, in this order and for different reasons.
///
/// Every live stream connection is **re-authenticated** against the set it
/// would get if it connected now. A connection holds the rights it
/// authenticated with, so without this the server would keep routing by the old
/// selection until the device reconnected — and the client would be looking at
/// a channel list that said otherwise.
///
/// Then `t-x-g-c` goes to the account's **other** devices (`compat/groups.md`
/// §3). Never to the device whose own action caused the change: the notice makes
/// a client discard every map item this server gave it and re-fetch, which would
/// undo what it had just done. `originating_uid` is [`None`] for an
/// administrator's change and for a caller that named no device, and then every
/// device is told — design 04 D9, where TAK Server would send nothing at all.
///
/// Returns how many connections were told. Never fails: a notice that could not
/// be sent is logged, because a client that missed one re-reads its channels on
/// its next connect and a request that was applied must not be reported as
/// having failed.
#[instrument("identity.members.channels_changed", skip_all, fields(user = %username))]
pub async fn channels_changed(
    context: &AppContext,
    user_id: UserId,
    username: &Username,
    originating_uid: Option<&str>,
) -> usize {
    context.events().channel_changed(username);

    if !context.has_live() {
        return 0;
    }

    let live = match context.live() {
        Ok(live) => live,
        Err(err) => {
            warn!(error = %err, "Could not reach the live connections after a channel change.");

            return 0;
        }
    };

    for (conn, device_id) in live.sessions_for_user(username) {
        if let Err(err) = reauth(context, &live, conn, device_id, user_id).await {
            warn!(error = %err, "Could not re-authenticate a live connection after a channel change.");
        }
    }

    live.groups_changed(username, originating_uid)
}

/// Replaces one live connection's effective channels with today's answer.
async fn reauth(
    context: &AppContext,
    live: &crate::stream::LiveState,
    conn: crate::stream::ConnId,
    device_id: Option<DeviceId>,
    user_id: UserId,
) -> Result<(), Error> {
    let db = context.db();

    let groups = match device_id {
        Some(device_id) => {
            effective_for_device(
                db,
                user_id,
                device_id,
                context.config().auth.anon_group_default,
            )
            .await?
        }
        // No device: the account-level selection is the whole answer.
        None => {
            effective_for_account(db, user_id, context.config().auth.anon_group_default).await?
        }
    };

    // The `OUT` names, which is what the contact and endpoint listings render.
    let names = groups.names(&db.groups().index().await?, Direction::Out);

    live.reauth(conn, std::sync::Arc::new(groups), names);

    Ok(())
}

/// Turns the names an administrator sent into channel identifiers.
fn resolve(
    groups: &[GroupRow],
    wanted: &[GroupMembership],
) -> Result<Vec<(GroupId, Direction)>, Error> {
    let mut resolved = Vec::new();

    for grant in wanted.iter().flat_map(GroupMembership::expand) {
        let group = groups
            .iter()
            .find(|group| group.name == grant.group)
            .ok_or_else(|| unknown_channel(&grant.group))?;

        if !resolved.contains(&(group.id, grant.direction)) {
            resolved.push((group.id, grant.direction));
        }
    }

    Ok(resolved)
}

/// Whether a grant is one an administrator made, and so ours to replace.
fn is_manual(membership: &&Membership) -> bool {
    membership.source == MembershipSource::Manual
}

/// The refusal for a channel name nobody here has.
fn unknown_channel(name: &GroupName) -> Error {
    human_errors::user(
        format!("There is no channel called '{name}'."),
        &[
            "Check the spelling, which is case sensitive.",
            "Create the channel first if it is meant to exist.",
        ],
    )
}

#[cfg(test)]
mod tests {
    // The active-state tests below drive [`active`](super::super::active)
    // through this module's re-exports, which is how every caller reaches it.
    use rustak_api::{ActiveGroup, GroupSource};

    use super::*;
    use crate::db::repos::{NewDevice, NewGroup, NewUser};

    struct Fixture {
        db: Database,
        user: UserId,
        device: DeviceId,
        anon: GroupRow,
        blue: GroupRow,
        red: GroupRow,
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
            .create(NewDevice::new(DeviceUid::parse("ANDROID-1").unwrap(), user))
            .await
            .unwrap()
            .id;

        let anon = db
            .groups()
            .get_by_name(&GroupName::anon())
            .await
            .unwrap()
            .expect("the migration seeds the default channel");
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
            user,
            device,
            anon,
            blue,
            red,
        }
    }

    fn grant(name: &str, direction: Direction) -> GroupMembership {
        GroupMembership::new(GroupName::parse(name).unwrap(), direction)
    }

    #[tokio::test]
    async fn replacing_grants_adds_what_is_asked_for_and_removes_what_is_not() {
        let f = fixture().await;

        let after = replace_manual(&f.db, f.user, &[grant("Blue", Direction::Both)], false)
            .await
            .unwrap();

        assert_eq!(after.len(), 2, "both directions are stored separately");
        assert!(after.iter().all(|held| held.group.as_str() == "Blue"));

        let after = replace_manual(&f.db, f.user, &[grant("Red", Direction::Out)], false)
            .await
            .unwrap();

        assert_eq!(after.len(), 1);
        assert_eq!(after[0].group.as_str(), "Red");
        assert_eq!(after[0].direction, Direction::Out);
    }

    #[tokio::test]
    async fn the_default_channel_survives_a_replacement_while_the_setting_is_on() {
        // An administrator taking `__ANON__` away would only be overruled the
        // next time anything asked, so the grant is put back rather than the
        // request being half-honoured.
        let f = fixture().await;

        let after = replace_manual(&f.db, f.user, &[grant("Blue", Direction::In)], true)
            .await
            .unwrap();

        assert_eq!(after.iter().filter(|held| held.group.is_anon()).count(), 2,);

        let without = replace_manual(&f.db, f.user, &[grant("Blue", Direction::In)], false)
            .await
            .unwrap();

        assert!(
            !without.iter().any(|held| held.group.is_anon()),
            "an installation that turned the default off grants by hand",
        );
    }

    #[tokio::test]
    async fn a_replacement_leaves_the_identity_providers_grants_alone() {
        // They are replaced wholesale at each sign-in, so removing one here
        // would last until the next one and no longer.
        let f = fixture().await;

        f.db.members()
            .grant(f.user, f.red.id, Direction::Out, MembershipSource::Oidc)
            .await
            .unwrap();

        let after = replace_manual(&f.db, f.user, &[grant("Blue", Direction::In)], false)
            .await
            .unwrap();

        assert!(
            after
                .iter()
                .any(|held| held.group.as_str() == "Red"
                    && held.source == Some(MembershipSource::Oidc)),
            "{after:?}",
        );
    }

    #[tokio::test]
    async fn a_channel_that_does_not_exist_is_refused_rather_than_dropped() {
        let f = fixture().await;

        let refused = replace_manual(&f.db, f.user, &[grant("Green", Direction::In)], false)
            .await
            .unwrap_err();

        assert!(refused.is(human_errors::Kind::User), "{refused}");
        assert!(refused.to_string().contains("Green"), "{refused}");
    }

    #[tokio::test]
    async fn a_grant_that_has_not_changed_is_not_rewritten() {
        // The delta is what keeps a member from being momentarily in nothing
        // while a live subscription is reading their channels.
        let f = fixture().await;
        replace_manual(&f.db, f.user, &[grant("Blue", Direction::Both)], false)
            .await
            .unwrap();

        let again = replace_manual(
            &f.db,
            f.user,
            &[grant("Blue", Direction::Both), grant("Red", Direction::In)],
            false,
        )
        .await
        .unwrap();

        assert_eq!(again.len(), 3);
        assert!(
            again
                .iter()
                .all(|held| held.source == Some(MembershipSource::Manual))
        );
    }

    #[tokio::test]
    async fn a_subscription_is_what_the_person_holds_minus_what_the_device_switched_off() {
        let f = fixture().await;
        replace_manual(
            &f.db,
            f.user,
            &[
                grant("Blue", Direction::Both),
                grant("Red", Direction::Both),
            ],
            false,
        )
        .await
        .unwrap();

        let all = effective_for_device(&f.db, f.user, f.device, false)
            .await
            .unwrap();
        assert!(all.contains(f.blue.bitpos, Direction::In));
        assert!(all.contains(f.red.bitpos, Direction::Out));

        set_active(
            &f.db,
            f.device,
            &[ActiveGroup {
                group: GroupName::parse("Red").unwrap(),
                direction: Direction::Both,
                active: false,
            }],
        )
        .await
        .unwrap();

        let narrowed = effective_for_device(&f.db, f.user, f.device, false)
            .await
            .unwrap();

        assert!(narrowed.contains(f.blue.bitpos, Direction::In));
        assert!(
            !narrowed.contains(f.red.bitpos, Direction::Out),
            "a channel the device switched off is not in its subscription",
        );
    }

    #[tokio::test]
    async fn the_default_channel_reaches_a_subscription_that_never_had_the_grant() {
        // An account created while the setting was off, then the setting turned
        // on: the subscription has to reflect what the installation now says.
        let f = fixture().await;

        let effective = effective_for_device(&f.db, f.user, f.device, true)
            .await
            .unwrap();

        assert!(effective.contains(f.anon.bitpos, Direction::In));
        assert!(effective.contains(f.anon.bitpos, Direction::Out));

        assert!(
            !effective_for_device(&f.db, f.user, f.device, false)
                .await
                .unwrap()
                .contains(f.anon.bitpos, Direction::In),
        );
    }

    #[tokio::test]
    async fn switching_the_default_channel_off_on_a_device_still_works() {
        // The setting says who holds the channel, not that a client may never
        // mute it.
        let f = fixture().await;

        set_active(
            &f.db,
            f.device,
            &[ActiveGroup {
                group: GroupName::anon(),
                direction: Direction::Out,
                active: false,
            }],
        )
        .await
        .unwrap();

        let effective = effective_for_device(&f.db, f.user, f.device, true)
            .await
            .unwrap();

        assert!(!effective.contains(f.anon.bitpos, Direction::Out));
        assert!(effective.contains(f.anon.bitpos, Direction::In));
    }

    #[tokio::test]
    async fn an_active_state_for_a_channel_that_has_gone_is_dropped_rather_than_refused() {
        // ATAK sends back the list it was given; a channel deleted since would
        // otherwise break every client that still had it cached.
        let f = fixture().await;

        let applied = set_active(
            &f.db,
            f.device,
            &[
                ActiveGroup {
                    group: GroupName::parse("Blue").unwrap(),
                    direction: Direction::In,
                    active: true,
                },
                ActiveGroup {
                    group: GroupName::parse("Vanished").unwrap(),
                    direction: Direction::In,
                    active: true,
                },
            ],
        )
        .await
        .unwrap();

        assert_eq!(applied, 1);
    }

    #[tokio::test]
    async fn a_devices_channel_state_reads_back_named_and_sorted() {
        let f = fixture().await;

        assert!(active_for_device(&f.db, f.device).await.unwrap().is_empty());

        set_active(
            &f.db,
            f.device,
            &[
                ActiveGroup {
                    group: GroupName::parse("Red").unwrap(),
                    direction: Direction::In,
                    active: false,
                },
                ActiveGroup {
                    group: GroupName::parse("Blue").unwrap(),
                    direction: Direction::Both,
                    active: true,
                },
            ],
        )
        .await
        .unwrap();

        let state = active_for_device(&f.db, f.device).await.unwrap();
        let names: Vec<&str> = state.iter().map(|held| held.group.as_str()).collect();

        assert_eq!(names, vec!["Blue", "Blue", "Red"]);
        assert!(!state.last().unwrap().active);
    }

    #[tokio::test]
    async fn the_grants_listing_names_every_channel_and_where_it_came_from() {
        let f = fixture().await;
        f.db.members()
            .grant(f.user, f.blue.id, Direction::In, MembershipSource::Manual)
            .await
            .unwrap();
        f.db.members()
            .grant(f.user, f.red.id, Direction::Out, MembershipSource::Oidc)
            .await
            .unwrap();

        let grants = grants_for_user(&f.db, f.user).await.unwrap();

        assert_eq!(
            grants
                .iter()
                .map(|held| (held.group.as_str(), held.direction, held.source))
                .collect::<Vec<_>>(),
            vec![
                ("Blue", Direction::In, Some(MembershipSource::Manual)),
                ("Red", Direction::Out, Some(MembershipSource::Oidc)),
            ],
        );

        assert_eq!(f.anon.source, GroupSource::System);
    }
}
