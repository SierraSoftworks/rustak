//! Who each published server event may be shown to.
//!
//! `GET /api/v1/events` is read by administrators *and* by services, and a
//! service is an ordinary account — normally one with no channel memberships at
//! all. Before R-01 H3 the feed was the one place in the server where that did
//! not matter: every subscriber received every device's uid and callsign, every
//! mission's name and guid, and every package's name, size and **hash**, which
//! is the download handle for `GET /Marti/sync/content?hash=`.
//!
//! The rest of the server already knows how to answer each of those questions —
//! [`Hub::snapshot_for`](crate::stream::Hub::snapshot_for) filters connections
//! by channel reachability, `packages::readable` answers an out-of-channel
//! package with the same `404` as a missing one, and the mission listing keeps
//! only the missions a [`Viewer`] may receive from. This module is those same
//! rules, expressed once so the bus can carry them.
//!
//! # Why the rule travels with the event rather than on the wire
//!
//! An [`Audience`] is attached where the event is published, by the code that
//! still has the channel list in its hand, and is **not** part of
//! [`ServerEvent`](rustak_api::event::ServerEvent). Putting the channels into
//! the payload would tell every reader which channels they were *not* in, and
//! would make correct filtering the consumer's job — which is the mistake this
//! finding was. The wire shape is unchanged; what changed is that an event a
//! subscriber may not see never reaches its socket.
//!
//! # Fail closed
//!
//! There is no `Default` and no implicit audience: every publisher names one.
//! A new event kind that forgets to is a compile error rather than a leak.

use std::sync::Arc;

use rustak_api::ServiceName;
use rustak_core::identity::{GroupSet, can_reach};
use rustak_core::prelude::*;

use crate::files::Viewer;

/// What a subscriber has to satisfy before one event is written to it.
#[derive(Clone, Debug)]
pub enum Audience {
    /// Anybody the feed lets in at all. For events that name nothing an
    /// installation keeps from its own operators and sidecars.
    Everyone,

    /// Only an administrator.
    Administrators,

    /// Anybody a principal holding these group rights could reach over the CoT
    /// stream — the rule `Hub::snapshot_for` applies to the client listing.
    Reachable(Arc<GroupSet>),

    /// Anybody who may **receive** from one of these channels. An empty list
    /// means `__ANON__`, which every principal holds; `Viewer::can_read_groups`
    /// is the same rule the package and mission listings use.
    Channels(Vec<String>),

    /// The named account, or an administrator.
    Account(Username),

    /// The named service, or an administrator.
    Service(ServiceName),
}

/// What one open feed may see, resolved when it opened and again on every
/// re-authorization.
#[derive(Clone, Debug)]
pub struct Subscriber {
    /// The account reading the feed.
    pub username: Username,

    /// Whether it administers the installation.
    pub is_admin: bool,

    /// Its effective group rights, for reachability.
    pub groups: Arc<GroupSet>,

    /// Its channels by name, for the resource rules.
    pub viewer: Viewer,

    /// The service registration it holds, when it is a sidecar rather than a
    /// person.
    pub service: Option<ServiceName>,
}

impl Subscriber {
    /// Whether this subscriber may be shown an event published to `audience`.
    pub fn may_see(&self, audience: &Audience) -> bool {
        // An administrator reads the whole installation everywhere else in the
        // API — `/api/v1/clients` and the file manager included — so the feed
        // would be an odd place to be narrower.
        if self.is_admin {
            return true;
        }

        match audience {
            Audience::Everyone => true,
            Audience::Administrators => false,
            Audience::Reachable(groups) => can_reach(groups, &self.groups),
            Audience::Channels(groups) => self.viewer.can_read_groups(groups),
            Audience::Account(username) => &self.username == username,
            Audience::Service(name) => self.service.as_ref() == Some(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::identity::Direction;

    use super::*;

    fn group_set(bits: &[u32], direction: Direction) -> Arc<GroupSet> {
        let mut set = GroupSet::new();

        for bit in bits {
            set.set(*bit, direction);
        }

        Arc::new(set)
    }

    fn subscriber(is_admin: bool, out: &[&str], bits: &[u32]) -> Subscriber {
        Subscriber {
            username: Username::parse("svc.weather").unwrap(),
            is_admin,
            groups: group_set(bits, Direction::Out),
            viewer: Viewer {
                username: Some("svc.weather".to_string()),
                is_admin,
                out_groups: out.iter().map(|name| (*name).to_string()).collect(),
                held_groups: out.iter().map(|name| (*name).to_string()).collect(),
            },
            service: Some(ServiceName::parse("weather").unwrap()),
        }
    }

    #[test]
    fn a_service_with_no_channels_sees_nothing_it_was_not_addressed() {
        // The reproduction in R-01 H3: a service account with no memberships
        // received every device and every package in the installation.
        let svc = subscriber(false, &[], &[]);

        assert!(!svc.may_see(&Audience::Administrators));
        assert!(!svc.may_see(&Audience::Channels(vec!["Blue".to_string()])));
        assert!(!svc.may_see(&Audience::Reachable(group_set(&[7], Direction::In))));
        assert!(!svc.may_see(&Audience::Account(Username::parse("ada").unwrap())));
    }

    #[test]
    fn a_service_still_hears_about_itself() {
        let svc = subscriber(false, &[], &[]);

        assert!(svc.may_see(&Audience::Everyone));
        assert!(svc.may_see(&Audience::Service(ServiceName::parse("weather").unwrap())));
        assert!(!svc.may_see(&Audience::Service(ServiceName::parse("adsb").unwrap())));
    }

    #[test]
    fn a_member_of_the_channel_sees_what_was_published_to_it() {
        let member = subscriber(false, &["Blue"], &[7]);

        assert!(member.may_see(&Audience::Channels(vec!["Blue".to_string()])));
        assert!(!member.may_see(&Audience::Channels(vec!["Red".to_string()])));
        assert!(member.may_see(&Audience::Reachable(group_set(&[7], Direction::In))));
        assert!(!member.may_see(&Audience::Reachable(group_set(&[9], Direction::In))));
    }

    #[test]
    fn an_upload_addressed_to_no_channel_reaches_everybody_the_way_anon_does() {
        let anon = subscriber(false, &[rustak_api::identity::GroupName::ANON], &[]);

        assert!(anon.may_see(&Audience::Channels(Vec::new())));
        assert!(
            !subscriber(false, &[], &[]).may_see(&Audience::Channels(Vec::new())),
            "an account that is not even in __ANON__ has no claim on it",
        );
    }

    #[test]
    fn an_administrator_reads_the_whole_installation() {
        let admin = subscriber(true, &[], &[]);

        assert!(admin.may_see(&Audience::Administrators));
        assert!(admin.may_see(&Audience::Channels(vec!["Red".to_string()])));
        assert!(admin.may_see(&Audience::Account(Username::parse("ada").unwrap())));
        assert!(admin.may_see(&Audience::Service(ServiceName::parse("adsb").unwrap())));
    }

    #[test]
    fn an_account_hears_about_its_own_channels_changing() {
        let mut own = subscriber(false, &[], &[]);
        own.username = Username::parse("ada").unwrap();

        assert!(own.may_see(&Audience::Account(Username::parse("ada").unwrap())));
        assert!(!own.may_see(&Audience::Account(Username::parse("grace").unwrap())));
    }
}
