//! Telling connected clients something that did not arrive on the wire.
//!
//! Three kinds of message the server originates rather than relays: the
//! disconnect notice that takes a peer off everybody's map, the group-change
//! notice that makes a client re-read its channels, and whatever a later
//! milestone needs to push at one device. [`Notifier`] is the seam other
//! modules take — the groups API, the mission store, the revocation hook — so
//! that none of them has to know what a [`Hub`] is.
//!
//! # These are sent, not queued
//!
//! Every method here returns how many connections it reached, and a connection
//! whose queue is full is one of the ones it did not. That is deliberate: a
//! notice is only meaningful to a client that is keeping up, and a client that
//! is not is about to be disconnected anyway — at which point it will reconnect
//! and be told everything from scratch.

use std::sync::Arc;

use rustak_cot::codec::EncodedEvent;
use rustak_cot::{CotTime, Event, msgs};

use crate::prelude::*;

use super::hub::Hub;
use super::liveness::LeaveReason;
use super::subscription::{ConnHandle, ConnId, Outbound, SendResult, Subscription};

/// Pushing a server-originated message at connected clients.
pub trait Notifier: Send + Sync {
    /// Sends to every connection claiming a `clientUid`.
    fn send_to_uid(&self, client_uid: &str, event: Event) -> usize;

    /// Sends to every connection authenticated as an account.
    fn send_to_user(&self, username: &Username, event: Event) -> usize;

    /// Sends one copy of a message to every connection claiming any of
    /// `client_uids`.
    ///
    /// A mission notice is addressed to a list rather than to one device:
    /// [`send_to_uid`](Self::send_to_uid) in a loop would encode the same event
    /// once per subscriber, which on a busy Data Sync is the whole cost.
    fn send_to_uids(&self, client_uids: &[String], event: Event) -> usize;

    /// Sends to every identified connection that receives from any of
    /// `groups`, skipping the connections claiming `except_uid`.
    ///
    /// The broadcast half of the mission notifications: "everyone who could
    /// read this mission, except whoever changed it". It is not the
    /// sender-to-receiver reachability question the broker asks — there is no
    /// sender here, only a mission and the channels it belongs to.
    fn broadcast_to_groups(
        &self,
        groups: &[GroupName],
        except_uid: Option<&str>,
        event: Event,
    ) -> usize;

    /// Sends to one connection.
    fn send_to_conn(&self, id: ConnId, event: Event) -> bool;

    /// Sends to everyone a principal may reach, optionally skipping one
    /// connection — the one whose action caused the notice.
    fn send_reachable_from(&self, from: ConnId, exclude_self: bool, event: Event) -> usize;

    /// Closes every connection authenticated with a certificate.
    fn disconnect_by_fingerprint(&self, fingerprint: &str) -> usize;
}

impl Notifier for Hub {
    fn send_to_uid(&self, client_uid: &str, event: Event) -> usize {
        deliver(&self.handles_for_uid(client_uid), event)
    }

    fn send_to_user(&self, username: &Username, event: Event) -> usize {
        deliver(&self.handles_for_user(username), event)
    }

    fn send_to_uids(&self, client_uids: &[String], event: Event) -> usize {
        let mut handles: Vec<ConnHandle> = Vec::new();

        for uid in client_uids {
            for handle in self.handles_for_uid(uid) {
                if !handles.iter().any(|held| held.id() == handle.id()) {
                    handles.push(handle);
                }
            }
        }

        deliver(&handles, event)
    }

    fn broadcast_to_groups(
        &self,
        groups: &[GroupName],
        except_uid: Option<&str>,
        event: Event,
    ) -> usize {
        // `__ANON__` is force-included so that a read-only account still hears
        // about a public mission, which is what TAK Server does by adding the
        // channel to every subscriber's vector rather than to the mission's.
        let everyone = groups.contains(&GroupName::anon());

        let uids: Vec<String> = self
            .snapshot()
            .into_iter()
            .filter(|peer| Some(peer.uid.as_str()) != except_uid)
            .filter(|peer| everyone || peer.groups.iter().any(|held| groups.contains(held)))
            .map(|peer| peer.uid)
            .collect();

        self.send_to_uids(&uids, event)
    }

    fn send_to_conn(&self, id: ConnId, event: Event) -> bool {
        match self.handle(id) {
            Some(handle) => deliver(std::slice::from_ref(&handle), event) == 1,
            None => false,
        }
    }

    fn send_reachable_from(&self, from: ConnId, exclude_self: bool, event: Event) -> usize {
        deliver(&self.reachable_from(from, exclude_self), event)
    }

    fn disconnect_by_fingerprint(&self, fingerprint: &str) -> usize {
        let handles = self.handles_for_fingerprint(fingerprint);

        for handle in &handles {
            handle.close(LeaveReason::Revoked);
        }

        if !handles.is_empty() {
            info!(
                connections = handles.len(),
                "Closed stream connections whose certificate was revoked."
            );
        }

        handles.len()
    }
}

/// Encodes once and hands the same `Arc` to every recipient.
fn deliver(handles: &[ConnHandle], event: Event) -> usize {
    if handles.is_empty() {
        return 0;
    }

    let encoded = Arc::new(EncodedEvent::new(event));

    handles
        .iter()
        .filter(|handle| handle.send(Outbound::Event(Arc::clone(&encoded))) == SendResult::Sent)
        .count()
}

/// Tells everyone a departing subscription could reach that it has gone.
///
/// Only worth sending for a subscription that had announced itself: a client
/// that never sent a callsign was never on anybody's map, so a notice removing
/// it would name a uid the recipients have never heard of
/// (`compat/streaming.md` §9).
///
/// Called **after** the subscription has been unregistered, so the reachability
/// is computed from the departing principal directly rather than from a hub
/// entry that is already gone.
pub fn on_disconnect(hub: &Hub, gone: &Subscription, uid: String, now: CotTime) -> usize {
    let (Some(client_uid), Some(_)) = (&gone.client_uid, &gone.callsign) else {
        return 0;
    };

    let last_sa_type = gone.last_sa_type.as_deref().unwrap_or("a-f-G");
    let notice = msgs::disconnect(uid, client_uid, last_sa_type, now);
    let handles = hub.handles_reachable_by(&gone.principal, Some(gone.id));

    let reached = deliver(&handles, notice);

    debug!(
        uid = %client_uid,
        peers = reached,
        "Told the peers of a departing client that it had gone."
    );

    reached
}

/// Tells an account's **other** devices that its channels changed.
///
/// Never the device whose own action caused the change: it already knows, and
/// both ATAK and CloudTAK react to the notice by discarding every map item this
/// server gave them and re-fetching, which would undo the change the client had
/// just made (`compat/streaming.md` §9).
pub fn on_groups_changed(
    hub: &Hub,
    username: &Username,
    originating_uid: Option<&str>,
    uid: String,
    now: CotTime,
) -> usize {
    let handles: Vec<ConnHandle> = match originating_uid {
        Some(originating) => {
            let excluded: Vec<ConnId> = hub
                .handles_for_uid(originating)
                .iter()
                .map(ConnHandle::id)
                .collect();

            hub.handles_for_user(username)
                .into_iter()
                .filter(|handle| !excluded.contains(&handle.id()))
                .collect()
        }
        None => hub.handles_for_user(username),
    };

    let notice = msgs::group_change(msgs::group_change_uid(&uid, originating_uid), now);

    deliver(&handles, notice)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustak_cot::detail::{Contact, Group, contact::STREAMING_ENDPOINT};
    use rustak_cot::types::cot_type;
    use tokio::sync::mpsc;

    use super::super::subscription::{ConnStats, Subscription};
    use super::*;

    fn principal(name: &str, grants: &[(u32, Direction)]) -> Arc<Principal> {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        Arc::new(
            Principal::new(
                UserId::from(1),
                Username::parse(name).unwrap(),
                PrincipalKind::Person,
                AuthMethod::SetupToken,
            )
            .with_groups(Arc::new(groups)),
        )
    }

    fn join(
        hub: &Hub,
        name: &str,
        grants: &[(u32, Direction)],
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        let id = hub.next_id();
        let (tx, rx) = mpsc::channel(8);

        hub.register(Subscription::new(
            id,
            principal(name, grants),
            Vec::new(),
            format!("{name:f>64}"),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));

        (id, rx)
    }

    /// The same, but carrying the channel **names** a contact listing shows —
    /// which is what a mission broadcast selects on.
    fn join_channels(
        hub: &Hub,
        name: &str,
        channels: &[&str],
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        let id = hub.next_id();
        let (tx, rx) = mpsc::channel(8);

        hub.register(Subscription::new(
            id,
            principal(name, &[(7, Direction::Both)]),
            channels
                .iter()
                .map(|channel| GroupName::parse(channel).unwrap())
                .collect(),
            format!("{name:f>64}"),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));

        (id, rx)
    }

    fn identify(hub: &Hub, id: ConnId, uid: &str, callsign: &str) {
        let event = Event::builder("a-f-G-U-C", uid)
            .point(51.5, -0.12)
            .stale_after(Duration::from_secs(60))
            .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
            .typed(&Group::new("Cyan", "Team Member"))
            .build();
        let encoded = Arc::new(EncodedEvent::new(event.clone()));

        hub.apply_event(id, &event, Some(&encoded));
    }

    fn received(rx: &mut mpsc::Receiver<Outbound>) -> Option<Event> {
        match rx.try_recv().ok()? {
            Outbound::Event(event) => Some(event.event().clone()),
            _ => None,
        }
    }

    #[test]
    fn a_departing_client_is_taken_off_the_maps_of_everyone_who_could_see_it() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&hub, "bravo", &[(7, Direction::Both)]);
        let (_stranger, mut stranger_rx) = join(&hub, "stranger", &[(9, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");

        let gone = hub.unregister(alpha).expect("the departing subscription");
        let reached = on_disconnect(&hub, &gone, "notice-1".into(), CotTime::now());

        assert_eq!(reached, 1);
        let notice = received(&mut bravo_rx).expect("bravo hears about it");
        assert_eq!(notice.r#type, cot_type::DISCONNECT);
        assert_eq!(
            notice.detail.find("link").and_then(|link| link.get("uid")),
            Some("UID-A"),
        );
        assert!(
            received(&mut stranger_rx).is_none(),
            "a peer in another channel never knew it was there",
        );
    }

    #[test]
    fn a_client_that_never_announced_itself_leaves_without_a_notice() {
        let hub = Hub::new();
        let (silent, _silent_rx) = join(&hub, "silent", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&hub, "bravo", &[(7, Direction::Both)]);

        let gone = hub.unregister(silent).expect("the departing subscription");

        assert_eq!(
            on_disconnect(&hub, &gone, "notice-1".into(), CotTime::now()),
            0
        );
        assert!(received(&mut bravo_rx).is_none());
    }

    #[test]
    fn a_channel_change_reaches_the_accounts_other_devices_only() {
        // The device that made the change already knows, and the notice makes a
        // client throw away everything this server gave it — so sending it back
        // would undo the change that had just been made.
        let hub = Hub::new();
        let (phone, mut phone_rx) = join(&hub, "alice", &[(7, Direction::Both)]);
        let (laptop, mut laptop_rx) = join(&hub, "alice", &[(7, Direction::Both)]);
        identify(&hub, phone, "UID-PHONE", "ALICE-P");
        identify(&hub, laptop, "UID-LAPTOP", "ALICE-L");

        let username = Username::parse("alice").unwrap();
        let reached = on_groups_changed(
            &hub,
            &username,
            Some("UID-PHONE"),
            "n-1".into(),
            CotTime::now(),
        );

        assert_eq!(reached, 1);
        assert!(received(&mut phone_rx).is_none());
        let notice = received(&mut laptop_rx).expect("the other device is told");
        assert_eq!(notice.r#type, cot_type::GROUP_CHANGE);
        assert_eq!(notice.uid, "n-1.UID-PHONE");
    }

    #[test]
    fn an_administrator_changing_membership_tells_every_device() {
        let hub = Hub::new();
        let (phone, mut phone_rx) = join(&hub, "alice", &[(7, Direction::Both)]);
        identify(&hub, phone, "UID-PHONE", "ALICE-P");

        let username = Username::parse("alice").unwrap();

        assert_eq!(
            on_groups_changed(&hub, &username, None, "n-2".into(), CotTime::now()),
            1
        );
        assert_eq!(received(&mut phone_rx).unwrap().uid, "n-2");
    }

    #[test]
    fn a_notice_addressed_to_several_uids_is_encoded_once_and_reaches_each() {
        let hub = Hub::new();
        let (alpha, mut alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (bravo, mut bravo_rx) = join(&hub, "bravo", &[(7, Direction::Both)]);
        let (_stranger, mut stranger_rx) = join(&hub, "stranger", &[(9, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");
        identify(&hub, bravo, "UID-B", "BRAVO");

        let notice = Event::builder("t-x-m-c", "n-1").point(0.0, 0.0).build();
        let reached = hub.send_to_uids(&["UID-A".into(), "UID-B".into()], notice);

        assert_eq!(reached, 2);
        assert!(received(&mut alpha_rx).is_some());
        assert!(received(&mut bravo_rx).is_some());
        assert!(
            received(&mut stranger_rx).is_none(),
            "a mission notice is addressed, not broadcast",
        );
    }

    #[test]
    fn a_uid_named_twice_is_still_told_once() {
        let hub = Hub::new();
        let (alpha, mut alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");

        let notice = Event::builder("t-x-m-c", "n-1").point(0.0, 0.0).build();

        assert_eq!(
            hub.send_to_uids(&["UID-A".into(), "UID-A".into()], notice),
            1
        );
        assert!(received(&mut alpha_rx).is_some());
        assert!(received(&mut alpha_rx).is_none());
    }

    #[test]
    fn a_mission_announcement_reaches_the_readers_of_its_channels_but_not_its_author() {
        let hub = Hub::new();
        let (author, mut author_rx) = join_channels(&hub, "author", &["Blue"]);
        let (reader, mut reader_rx) = join_channels(&hub, "reader", &["Blue"]);
        let (outsider, mut outsider_rx) = join_channels(&hub, "outsider", &["Red"]);
        identify(&hub, author, "UID-AUTHOR", "AUTHOR");
        identify(&hub, reader, "UID-READER", "READER");
        identify(&hub, outsider, "UID-OUTSIDER", "OUTSIDER");

        let notice = Event::builder("t-x-m-n", "n-1").point(0.0, 0.0).build();
        let reached = hub.broadcast_to_groups(
            &[GroupName::parse("Blue").unwrap()],
            Some("UID-AUTHOR"),
            notice,
        );

        assert_eq!(reached, 1);
        assert!(received(&mut reader_rx).is_some());
        assert!(
            received(&mut author_rx).is_none(),
            "the creator already knows",
        );
        assert!(received(&mut outsider_rx).is_none());
    }

    #[test]
    fn a_public_mission_is_announced_to_everyone_connected() {
        // `__ANON__` is force-included so a read-only account still hears about
        // a public mission it holds no other channel for.
        let hub = Hub::new();
        let (reader, mut reader_rx) = join_channels(&hub, "reader", &["Red"]);
        identify(&hub, reader, "UID-READER", "READER");

        let notice = Event::builder("t-x-m-n", "n-1").point(0.0, 0.0).build();

        assert_eq!(
            hub.broadcast_to_groups(&[GroupName::anon()], None, notice),
            1
        );
        assert!(received(&mut reader_rx).is_some());
    }

    #[test]
    fn a_client_that_never_announced_itself_is_not_a_broadcast_recipient() {
        // The contact snapshot only lists subscriptions with a uid, and a
        // notice naming no uid could not be addressed anyway.
        let hub = Hub::new();
        let (_silent, mut silent_rx) = join_channels(&hub, "silent", &["Blue"]);

        let notice = Event::builder("t-x-m-n", "n-1").point(0.0, 0.0).build();

        assert_eq!(
            hub.broadcast_to_groups(&[GroupName::parse("Blue").unwrap()], None, notice),
            0
        );
        assert!(received(&mut silent_rx).is_none());
    }

    #[test]
    fn revoking_a_certificate_closes_what_it_bought() {
        let hub = Hub::new();
        let (_alpha, mut alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);

        let closed = hub.disconnect_by_fingerprint(&format!("{:f>64}", "alpha"));

        assert_eq!(closed, 1);
        assert!(matches!(alpha_rx.try_recv(), Ok(Outbound::Close)));
    }

    #[test]
    fn a_fingerprint_nothing_is_connected_with_closes_nothing() {
        let hub = Hub::new();

        assert_eq!(hub.disconnect_by_fingerprint(&"0".repeat(64)), 0);
    }
}
