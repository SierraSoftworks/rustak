//! What a channel looks like on the TAK wire, and which of them are switched on.
//!
//! [`groups`](super::groups) is the route file; this is the half that reads and
//! writes. It is separate because three route files need it —
//! `/Marti/api/groups/*`, `/Marti/api/clientEndPoints`' `group=` filter and the
//! subscription listings — and because the two files together are over
//! `conventions.md`'s 300-line limit.
//!
//! # Two places say whether a channel is on
//!
//! A membership is a *right*; whether it is currently switched on is a
//! *preference*, and rustak scopes that preference to a device
//! (`identity::members`) so that switching a channel off on a phone does not
//! switch it off on a laptop. But `PUT /Marti/api/groups/active` arrives with no
//! `clientUid` from every caller that is not an end-user device — CloudTAK's
//! browser half, a script, the admin UI — and those callers have no device row
//! to write to.
//!
//! So there is a second, account-level selection, kept in `user_group_state`
//! (migration `0012`) and used as the default a device inherits when it has
//! said nothing itself. A write with no `clientUid` sets the account default
//! **and** every one of that account's devices — the devices because a client
//! that *has* an opinion should be given the new one rather than silently
//! inheriting it, and the account because that is what a device enrolled
//! afterwards reads.
//!
//! The table matters more than where it puts the bytes. `members::effective`
//! joins it, so the routing path and this file now answer the same question the
//! same way: a device enrolled after an account-level change routes on the
//! account's selection from its very first connection, instead of routing
//! permissively until it called this endpoint.

use std::collections::HashMap;

use rustak_api::{ActiveGroup, Direction, GroupName};

use crate::auth::resolve::Resolved;
use crate::db::repos::GroupRow;
use crate::identity::{devices, members};
use crate::prelude::*;

use super::error::MartiError;
use super::time;

/// Every channel rustak serves is one it created itself.
///
/// TAK Server's other value is `LDAP`, for a channel mirrored out of a
/// directory. rustak has no LDAP source, so a client that branches on this only
/// ever sees the one answer (`compat/groups.md` §1).
pub const SYSTEM: &str = "SYSTEM";

/// One channel, in the shape ATAK's `ServerGroup` parser and CloudTAK's
/// TypeBox schema both read.
///
/// `bitpos`, `created`, `type` and `direction` are never omitted: ATAK drops
/// the **whole** channel when any of them is missing, and a negative `bitpos`
/// is treated the same way (`compat/groups.md` §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupJson {
    pub name: String,

    /// `IN` (may publish into it) or `OUT` (may receive from it). Never `BOTH`:
    /// storage holds one row per direction and so does the wire.
    pub direction: &'static str,

    /// `yyyy-MM-dd`, date only — **not** the instant format every other Marti
    /// timestamp uses. ATAK parses this field with a date-only pattern.
    pub created: String,

    #[serde(rename = "type")]
    pub kind: &'static str,

    pub bitpos: u32,

    pub active: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl GroupJson {
    /// One row of the answer, from a channel and the caller's state for it.
    fn new(row: &GroupRow, direction: Direction, active: bool) -> Self {
        Self {
            name: row.name.to_string(),
            direction: direction.as_str(),
            created: time::group_date(row.created_at),
            kind: SYSTEM,
            bitpos: row.bitpos,
            active,
            description: row.description.clone(),
        }
    }
}

/// Which channels a caller currently has switched on.
///
/// Two layers, most specific first: what the device said, then what the account
/// said, then on — because a client that has never called the channels endpoint
/// expects everything it is entitled to.
#[derive(Debug, Default, Clone)]
pub struct Selection {
    account: HashMap<(String, Direction), bool>,
    device: HashMap<(String, Direction), bool>,
}

impl Selection {
    /// Whether this channel is on for the caller the selection was read for.
    pub fn is_active(&self, name: &GroupName, direction: Direction) -> bool {
        let key = (name.to_string(), direction);

        self.device
            .get(&key)
            .or_else(|| self.account.get(&key))
            .copied()
            .unwrap_or(true)
    }
}

/// Reads the selection for an account, narrowed by a device when there is one.
///
/// # Errors
///
/// [`MartiError::Internal`] when a read fails.
pub async fn selection(
    context: &AppContext,
    user_id: UserId,
    device: Option<&DeviceUid>,
) -> Result<Selection, MartiError> {
    let mut selection = Selection {
        account: index(&members::active_for_user(context.db(), user_id).await?),
        device: HashMap::new(),
    };

    let Some(uid) = device else {
        return Ok(selection);
    };

    if let Some(row) = devices::get(context.db(), uid).await?
        && row.user_id == user_id
    {
        selection.device = index(&members::active_for_device(context.db(), row.id).await?);
    }

    Ok(selection)
}

/// Every channel a caller may see, with the direction and the active flag.
///
/// An administrator is shown every channel in both directions — they may grant
/// themselves any of them, and the Channels UI they administer from would
/// otherwise be able to display only what they had already joined. Everybody
/// else is shown exactly their grants, one row per (channel, direction).
///
/// # Errors
///
/// [`MartiError::Internal`] when a read fails.
pub async fn visible(context: &AppContext, who: &Resolved) -> Result<Vec<GroupJson>, MartiError> {
    let selection = selection(context, who.user.id, who.principal.device.as_ref()).await?;

    views(context, who.user.id, who.principal.is_admin, &selection).await
}

/// As [`visible`], for an account an administrator named rather than the caller.
///
/// Read without a device, because an administrator asking about somebody else
/// is asking about the account rather than about one of its phones.
///
/// # Errors
///
/// [`MartiError::Internal`] when a read fails.
pub async fn visible_to(
    context: &AppContext,
    user_id: UserId,
) -> Result<Vec<GroupJson>, MartiError> {
    let selection = selection(context, user_id, None).await?;

    views(context, user_id, false, &selection).await
}

/// The channel rows this installation has, newest last.
///
/// # Errors
///
/// [`MartiError::Internal`] when the read fails.
pub async fn rows(context: &AppContext) -> Result<Vec<GroupRow>, MartiError> {
    Ok(context.db().groups().list().await?)
}

/// Records a caller's new selection.
///
/// `client_uid` names the device that asked, and is the only case where one
/// device's state changes on its own. Without it the change is the account's:
/// the stored default is replaced *and* applied to every device the account
/// has, so that a selection made from a browser reaches the phones as well.
///
/// # Errors
///
/// [`MartiError::Internal`] when a read or write fails.
pub async fn apply(
    context: &AppContext,
    user_id: UserId,
    states: &[ActiveGroup],
    client_uid: Option<&str>,
) -> Result<(), MartiError> {
    if let Some(row) = named_device(context, user_id, client_uid).await? {
        members::set_active(context.db(), row.id, states).await?;

        return Ok(());
    }

    members::set_active_for_user(context.db(), user_id, states).await?;

    for row in devices::list_for_user(context.db(), user_id).await? {
        members::set_active(context.db(), row.id, states).await?;
    }

    Ok(())
}

/// The device a `clientUid` names, when it names one of this account's.
async fn named_device(
    context: &AppContext,
    user_id: UserId,
    client_uid: Option<&str>,
) -> Result<Option<crate::db::repos::DeviceRow>, MartiError> {
    let Some(uid) = client_uid.and_then(|uid| DeviceUid::parse(uid).ok()) else {
        return Ok(None);
    };

    match devices::get(context.db(), &uid).await? {
        // Somebody else's device is not this caller's to switch off, and a uid
        // we have never seen is a client that has not enrolled through us —
        // both fall back to the account selection rather than refusing, because
        // ATAK sends whatever uid it is configured with.
        Some(row) if row.user_id == user_id => Ok(Some(row)),
        _ => {
            debug!(
                uid = client_uid,
                "A channel selection named a device this account does not have."
            );

            Ok(None)
        }
    }
}

/// Builds the rows of the answer.
async fn views(
    context: &AppContext,
    user_id: UserId,
    is_admin: bool,
    selection: &Selection,
) -> Result<Vec<GroupJson>, MartiError> {
    let rows = rows(context).await?;
    let mut held: Vec<(GroupName, Direction)> = Vec::new();

    if is_admin {
        for row in &rows {
            for direction in Direction::Both.expand() {
                held.push((row.name.clone(), *direction));
            }
        }
    } else {
        for grant in members::grants_for_user(context.db(), user_id).await? {
            for single in grant.direction.expand() {
                held.push((grant.group.clone(), *single));
            }
        }
    }

    let mut views: Vec<GroupJson> = held
        .iter()
        .filter_map(|(name, direction)| {
            rows.iter()
                .find(|row| &row.name == name)
                .map(|row| GroupJson::new(row, *direction, selection.is_active(name, *direction)))
        })
        .collect();

    views.sort_by(|left, right| (&left.name, left.direction).cmp(&(&right.name, right.direction)));
    views.dedup_by(|left, right| left.name == right.name && left.direction == right.direction);

    Ok(views)
}

/// Turns a list of states into the lookup [`Selection`] answers from.
fn index(states: &[ActiveGroup]) -> HashMap<(String, Direction), bool> {
    states
        .iter()
        .flat_map(ActiveGroup::expand)
        .map(|state| ((state.group.to_string(), state.direction), state.active))
        .collect()
}

#[cfg(test)]
mod tests {
    use rustak_api::identity::GroupName;

    use super::*;
    use crate::db::repos::NewGroup;
    use crate::testing::TestServer;

    async fn channel(server: &TestServer, name: &str) {
        server
            .db()
            .groups()
            .create(NewGroup::manual(GroupName::parse(name).unwrap()))
            .await
            .unwrap();
    }

    fn state(name: &str, direction: Direction, active: bool) -> ActiveGroup {
        ActiveGroup {
            group: GroupName::parse(name).unwrap(),
            direction,
            active,
        }
    }

    #[actix_web::test]
    async fn a_channel_nobody_has_an_opinion_about_is_switched_on() {
        // A client that has never called the endpoint expects everything it is
        // entitled to, so the default has to be on rather than off.
        let selection = Selection::default();

        assert!(selection.is_active(&GroupName::anon(), Direction::In));
    }

    #[actix_web::test]
    async fn the_device_overrides_the_account() {
        let selection = Selection {
            account: index(&[state("Blue", Direction::Both, false)]),
            device: index(&[state("Blue", Direction::In, true)]),
        };

        assert!(selection.is_active(&GroupName::parse("Blue").unwrap(), Direction::In,));
        assert!(
            !selection.is_active(&GroupName::parse("Blue").unwrap(), Direction::Out),
            "the direction the device said nothing about still follows the account",
        );
    }

    #[actix_web::test]
    async fn a_selection_made_without_a_device_is_what_the_account_reads_back() {
        // The case every caller that is not an end-user device hits: CloudTAK's
        // browser half, the admin UI and a script all send no `clientUid`.
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        channel(&server, "Blue").await;

        apply(
            &server.context,
            user.id,
            &[state("Blue", Direction::Both, false)],
            None,
        )
        .await
        .unwrap();

        let selection = selection(&server.context, user.id, None).await.unwrap();

        assert!(!selection.is_active(&GroupName::parse("Blue").unwrap(), Direction::In));
        assert!(!selection.is_active(&GroupName::parse("Blue").unwrap(), Direction::Out));
    }

    #[actix_web::test]
    async fn an_administrator_sees_every_channel_in_both_directions() {
        let server = TestServer::start().await;
        let user = server.user("grace", true).await;
        channel(&server, "Blue").await;

        let selection = Selection::default();
        let views = views(&server.context, user.id, true, &selection)
            .await
            .unwrap();

        let named: Vec<(&str, &str)> = views
            .iter()
            .map(|view| (view.name.as_str(), view.direction))
            .collect();

        assert!(named.contains(&("Blue", "IN")));
        assert!(named.contains(&("Blue", "OUT")));
        assert!(named.contains(&("__ANON__", "IN")));
    }

    #[actix_web::test]
    async fn an_ordinary_account_sees_only_its_grants() {
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        channel(&server, "Blue").await;

        let views = views(&server.context, user.id, false, &Selection::default())
            .await
            .unwrap();

        assert!(
            views.iter().all(|view| view.name == "__ANON__"),
            "{views:?}",
        );
        assert!(views.iter().all(|view| view.bitpos > 0));
        assert!(views.iter().all(|view| view.created.len() == 10));
    }

    #[actix_web::test]
    async fn an_account_level_selection_reaches_the_devices_that_already_exist() {
        // The browser-half case: no `clientUid`, so the account default is set
        // and every device the account has is brought with it, because routing
        // reads the device rows first.
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        channel(&server, "Blue").await;

        let device = server
            .db()
            .devices()
            .create(crate::db::repos::NewDevice::new(
                DeviceUid::parse("ANDROID-ADA").unwrap(),
                user.id,
            ))
            .await
            .unwrap();

        apply(
            &server.context,
            user.id,
            &[state("Blue", Direction::Both, false)],
            None,
        )
        .await
        .unwrap();

        let selection = selection(&server.context, user.id, Some(&device.uid))
            .await
            .unwrap();

        assert!(!selection.is_active(&GroupName::parse("Blue").unwrap(), Direction::Out));
        assert_eq!(
            members::active_for_device(server.db(), device.id)
                .await
                .unwrap()
                .len(),
            2,
            "the device has its own rows, not only the account's",
        );
    }

    #[actix_web::test]
    async fn a_device_enrolled_after_an_account_level_change_reads_the_account() {
        // The gap `user_group_state` closes: this device has no rows of its own
        // and must still read — and route on — the account's answer.
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        channel(&server, "Blue").await;

        apply(
            &server.context,
            user.id,
            &[state("Blue", Direction::Both, false)],
            None,
        )
        .await
        .unwrap();

        let later = server
            .db()
            .devices()
            .create(crate::db::repos::NewDevice::new(
                DeviceUid::parse("ANDROID-LATER").unwrap(),
                user.id,
            ))
            .await
            .unwrap();

        let selection = selection(&server.context, user.id, Some(&later.uid))
            .await
            .unwrap();

        assert!(!selection.is_active(&GroupName::parse("Blue").unwrap(), Direction::Out));
        assert!(
            members::active_for_device(server.db(), later.id)
                .await
                .unwrap()
                .is_empty(),
            "it inherits rather than having rows written for it",
        );
    }

    #[actix_web::test]
    async fn a_channel_that_is_not_here_is_dropped_from_the_answer() {
        // A channel deleted between a client caching the list and sending it
        // back must not become a row with no bit position.
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;

        apply(
            &server.context,
            user.id,
            &[state("Vanished", Direction::Both, false)],
            None,
        )
        .await
        .unwrap();

        let views = views(
            &server.context,
            user.id,
            false,
            &selection(&server.context, user.id, None).await.unwrap(),
        )
        .await
        .unwrap();

        assert!(views.iter().all(|view| view.name != "Vanished"));
    }
}
