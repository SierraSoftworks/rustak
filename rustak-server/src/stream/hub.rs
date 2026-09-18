//! The registry of live connections, and every question routing asks of it.
//!
//! One [`parking_lot::RwLock`] over a private registry, and a rule: **no `.await`
//! while a guard is alive**. Every method here takes the lock, does a bounded
//! amount of map work, and returns *owned* data — a `Vec<ConnHandle>`, an
//! `Arc<Principal>`, a `Vec<ClientEndpoint>`. Delivery happens outside, on the
//! sending connection's own task.
//!
//! # Why not one actor
//!
//! A single hub task would serialise every inbound message through one mailbox
//! and put an `await` on the hot path of a server whose whole job is fan-out.
//! The ordering guarantees the protocol actually needs — replay before the
//! negotiation offer, the negotiation answer before the mode switch, the
//! disconnect notice after the unregister — are all *per connection*, and the
//! per-connection writer channel gives them without a global ordering anybody
//! has to wait on.
//!
//! # The indexes
//!
//! `<marti><dest callsign>` and `<dest uid>` are addressed by strings the
//! client chooses, and both are looked up once per message, so both are indexed.
//! Two clients can legitimately share a callsign (a spare radio, a device
//! handed over), so an index entry is a list rather than a single id.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use rustak_cot::Event;
use rustak_cot::codec::{EncodedEvent, Mode};

use crate::prelude::*;

use super::registry::{Index, Registry};
use super::subscription::{ClientEndpoint, ConnHandle, ConnId, SaUpdate, Subscription};

/// Every connection this server is currently carrying.
#[derive(Debug, Default)]
pub struct Hub {
    inner: RwLock<Registry>,
    next_id: AtomicU64,
}

impl Hub {
    /// An empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// The identifier the next connection will be registered under.
    pub fn next_id(&self) -> ConnId {
        ConnId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// Adds a connection.
    pub fn register(&self, subscription: Subscription) -> ConnId {
        let id = subscription.id;
        let mut registry = self.inner.write();

        registry.index(&subscription);
        registry.conns.insert(id, subscription);

        id
    }

    /// Removes a connection and hands back what it was, so the caller can send
    /// the disconnect notice its identity decides.
    pub fn unregister(&self, id: ConnId) -> Option<Subscription> {
        let mut registry = self.inner.write();
        let subscription = registry.conns.remove(&id)?;

        registry.unindex(&subscription);

        Some(subscription)
    }

    /// How many connections are registered.
    pub fn len(&self) -> usize {
        self.inner.read().conns.len()
    }

    /// Whether nothing is connected.
    pub fn is_empty(&self) -> bool {
        self.inner.read().conns.is_empty()
    }

    /// Folds an inbound event into its sender's record, re-indexing if the
    /// event fixed the identity.
    pub fn apply_event(
        &self,
        id: ConnId,
        event: &Event,
        encoded: Option<&Arc<EncodedEvent>>,
    ) -> Option<SaUpdate> {
        let mut registry = self.inner.write();
        let subscription = registry.conns.get_mut(&id)?;
        let update = subscription.apply_event(event, encoded);

        if update.first_identity {
            // Cloned out because `index` needs the registry mutably and the
            // subscription is borrowed from it.
            let uid = subscription.client_uid.clone();
            let callsign = subscription.callsign.clone();

            if let Some(uid) = uid {
                registry.by_uid.entry(uid).or_default().push(id);
            }
            if let Some(callsign) = callsign {
                registry.by_callsign.entry(callsign).or_default().push(id);
            }
        }

        Some(update)
    }

    /// Sets or clears the incognito flag.
    pub fn set_incognito(&self, id: ConnId, incognito: bool) -> bool {
        let mut registry = self.inner.write();

        match registry.conns.get_mut(&id) {
            Some(subscription) => {
                subscription.incognito = incognito;
                true
            }
            None => false,
        }
    }

    /// Records which encoding a connection has switched to.
    pub fn set_mode(&self, id: ConnId, mode: Mode) {
        if let Some(subscription) = self.inner.write().conns.get_mut(&id) {
            subscription.mode = mode;
        }
    }

    /// Whether a connection is invisible to implicit broadcast.
    pub fn is_incognito(&self, id: ConnId) -> bool {
        self.inner
            .read()
            .conns
            .get(&id)
            .is_some_and(|subscription| subscription.incognito)
    }

    /// The delivery handle for one connection.
    pub fn handle(&self, id: ConnId) -> Option<ConnHandle> {
        self.inner
            .read()
            .conns
            .get(&id)
            .map(|subscription| subscription.handle.clone())
    }

    /// Who a connection is, and what it may reach.
    pub fn principal(&self, id: ConnId) -> Option<Arc<Principal>> {
        self.inner
            .read()
            .conns
            .get(&id)
            .map(|subscription| Arc::clone(&subscription.principal))
    }

    /// The device row a connection's certificate was issued to.
    pub fn device_id(&self, id: ConnId) -> Option<DeviceId> {
        self.inner.read().conns.get(&id)?.device_id
    }

    /// The `clientUid` a connection has claimed, if it has sent one.
    pub fn client_uid(&self, id: ConnId) -> Option<String> {
        self.inner.read().conns.get(&id)?.client_uid.clone()
    }

    /// Every connection a message from `sender` is allowed to reach.
    ///
    /// `exclude_self` is the implicit-broadcast rule: a client's own position
    /// report must not come back to it, but an explicitly addressed message
    /// may (`compat/streaming.md` §8).
    pub fn reachable_from(&self, sender: ConnId, exclude_self: bool) -> Vec<ConnHandle> {
        let registry = self.inner.read();
        let Some(from) = registry.conns.get(&sender) else {
            return Vec::new();
        };

        registry
            .conns
            .values()
            .filter(|candidate| !(exclude_self && candidate.id == sender))
            .filter(|candidate| can_reach(&from.principal.groups, &candidate.principal.groups))
            .map(|candidate| candidate.handle.clone())
            .collect()
    }

    /// Every connection a principal that is no longer registered may reach.
    ///
    /// The disconnect notice needs this: by the time it is sent the departing
    /// subscription has already been removed, which is what stops it being sent
    /// its own obituary and what makes the notice truthful about who is left.
    pub fn handles_reachable_by(
        &self,
        from: &Principal,
        exclude: Option<ConnId>,
    ) -> Vec<ConnHandle> {
        self.inner
            .read()
            .conns
            .values()
            .filter(|candidate| Some(candidate.id) != exclude)
            .filter(|candidate| can_reach(&from.groups, &candidate.principal.groups))
            .map(|candidate| candidate.handle.clone())
            .collect()
    }

    /// The connections named by a list of callsigns, filtered by reachability.
    pub fn resolve_callsigns(&self, sender: ConnId, callsigns: &[String]) -> Vec<ConnHandle> {
        self.resolve(sender, callsigns, Index::Callsign)
    }

    /// The connections named by a list of client uids, filtered by
    /// reachability.
    pub fn resolve_uids(&self, sender: ConnId, uids: &[String]) -> Vec<ConnHandle> {
        self.resolve(sender, uids, Index::Uid)
    }

    /// Every connection holding `bitpos` in the `OUT` direction that `sender`
    /// may also reach.
    pub fn reachable_in_group(&self, sender: ConnId, bitpos: u32) -> Vec<ConnHandle> {
        let registry = self.inner.read();
        let Some(from) = registry.conns.get(&sender) else {
            return Vec::new();
        };

        registry
            .conns
            .values()
            .filter(|candidate| candidate.id != sender)
            .filter(|candidate| candidate.principal.has_group(bitpos, Direction::Out))
            .filter(|candidate| can_reach(&from.principal.groups, &candidate.principal.groups))
            .map(|candidate| candidate.handle.clone())
            .collect()
    }

    /// The latest position of everyone `receiver` is allowed to see.
    ///
    /// Sent to a client the moment it connects, so that it does not have to
    /// wait up to a reporting interval to see anybody. Incognito peers are
    /// skipped: their whole point is not to appear unasked.
    pub fn latest_sa_for(&self, receiver: ConnId) -> Vec<Arc<EncodedEvent>> {
        let registry = self.inner.read();
        let Some(to) = registry.conns.get(&receiver) else {
            return Vec::new();
        };

        let mut replay: Vec<(ConnId, Arc<EncodedEvent>)> = registry
            .conns
            .values()
            .filter(|peer| peer.id != receiver && !peer.incognito)
            .filter(|peer| can_reach(&peer.principal.groups, &to.principal.groups))
            .filter_map(|peer| peer.latest_sa.clone().map(|event| (peer.id, event)))
            .collect();

        // Registration order, so a client sees the map fill in the order the
        // people on it arrived rather than in hash order.
        replay.sort_by_key(|(id, _)| *id);

        replay.into_iter().map(|(_, event)| event).collect()
    }

    /// Every connection claiming a given `clientUid`.
    pub fn handles_for_uid(&self, uid: &str) -> Vec<ConnHandle> {
        let registry = self.inner.read();

        registry
            .by_uid
            .get(uid)
            .map(|ids| registry.handles(ids))
            .unwrap_or_default()
    }

    /// Every connection authenticated as a given account.
    pub fn handles_for_user(&self, username: &Username) -> Vec<ConnHandle> {
        self.inner
            .read()
            .conns
            .values()
            .filter(|subscription| &subscription.principal.username == username)
            .map(|subscription| subscription.handle.clone())
            .collect()
    }

    /// Every connection authenticated with a given certificate.
    ///
    /// The revocation hook's question: a certificate that has been taken back
    /// must not leave a session open that it bought.
    pub fn handles_for_fingerprint(&self, fingerprint: &str) -> Vec<ConnHandle> {
        self.inner
            .read()
            .conns
            .values()
            .filter(|subscription| subscription.fingerprint == fingerprint)
            .map(|subscription| subscription.handle.clone())
            .collect()
    }

    /// Everyone connected, for `/Marti/api/clientEndPoints` and
    /// `/Marti/api/contacts/all`.
    ///
    /// Only the subscriptions that have identified themselves: a connection
    /// that has never sent a callsign has nothing a contact list could show.
    pub fn snapshot(&self) -> Vec<ClientEndpoint> {
        let registry = self.inner.read();
        let mut endpoints: Vec<ClientEndpoint> = registry
            .conns
            .values()
            .filter_map(Subscription::endpoint)
            .collect();

        endpoints.sort_by(|left, right| left.callsign.cmp(&right.callsign));

        endpoints
    }

    /// The subscriptions a reader may see, narrowed to what `viewer` can reach.
    pub fn snapshot_for(&self, viewer: &Principal) -> Vec<ClientEndpoint> {
        let registry = self.inner.read();
        let mut endpoints: Vec<ClientEndpoint> = registry
            .conns
            .values()
            .filter(|peer| !peer.incognito)
            .filter(|peer| can_reach(&peer.principal.groups, &viewer.groups))
            .filter_map(Subscription::endpoint)
            .collect();

        endpoints.sort_by(|left, right| left.callsign.cmp(&right.callsign));

        endpoints
    }

    /// The shared body of the two explicit-address lookups.
    fn resolve(&self, sender: ConnId, keys: &[String], index: Index) -> Vec<ConnHandle> {
        let registry = self.inner.read();
        let Some(from) = registry.conns.get(&sender) else {
            return Vec::new();
        };

        let mut handles = Vec::new();
        let mut seen: Vec<ConnId> = Vec::new();

        for key in keys {
            let Some(ids) = index.lookup(&registry, key) else {
                continue;
            };

            for id in ids {
                let Some(candidate) = registry.conns.get(id) else {
                    continue;
                };

                // Explicit addressing does not bypass the channel rules: a
                // callsign somebody is not allowed to talk to is a callsign
                // they cannot address either.
                if !can_reach(&from.principal.groups, &candidate.principal.groups) {
                    continue;
                }

                if seen.contains(id) {
                    continue;
                }

                seen.push(*id);
                handles.push(candidate.handle.clone());
            }
        }

        handles
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustak_cot::detail::{Contact, Group, contact::STREAMING_ENDPOINT};
    use tokio::sync::mpsc;

    use super::super::subscription::{ConnStats, Outbound};
    use super::*;

    /// A principal holding the listed bit positions in the listed directions.
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
                AuthMethod::ClientCert {
                    fingerprint: format!("{name:f>64}"),
                    serial: "0f".repeat(8),
                },
            )
            .with_groups(Arc::new(groups)),
        )
    }

    /// Registers a connection and hands back its id and the queue it writes to.
    fn join(
        hub: &Hub,
        name: &str,
        grants: &[(u32, Direction)],
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        let id = hub.next_id();
        let (tx, rx) = mpsc::channel(32);
        let handle = ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new());

        hub.register(Subscription::new(
            id,
            principal(name, grants),
            Vec::new(),
            format!("{name:f>64}"),
            "127.0.0.1:9000".parse().unwrap(),
            handle,
        ));

        (id, rx)
    }

    fn sa(uid: &str, callsign: &str) -> Event {
        Event::builder("a-f-G-U-C", uid)
            .how("m-g")
            .point(51.5, -0.12)
            .stale_after(Duration::from_secs(60))
            .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
            .typed(&Group::new("Cyan", "Team Member"))
            .build()
    }

    fn identify(hub: &Hub, id: ConnId, uid: &str, callsign: &str) {
        let event = sa(uid, callsign);
        let encoded = Arc::new(EncodedEvent::new(event.clone()));

        hub.apply_event(id, &event, Some(&encoded));
    }

    #[test]
    fn reachability_is_the_senders_in_against_the_receivers_out() {
        // The rule everything else depends on, asserted through the hub rather
        // than through `can_reach`, because the hub is what routing calls.
        let hub = Hub::new();
        let (sensor, _sensor_rx) = join(&hub, "sensor", &[(7, Direction::In)]);
        let (watch, _watch_rx) = join(&hub, "watch", &[(7, Direction::Out)]);

        assert_eq!(hub.reachable_from(sensor, true).len(), 1);
        assert!(
            hub.reachable_from(watch, true).is_empty(),
            "a receive-only subscription reaches nobody",
        );
    }

    #[test]
    fn a_broadcast_never_comes_back_to_the_sender() {
        let hub = Hub::new();
        let (alpha, _rx) = join(&hub, "alpha", &[(7, Direction::Both)]);

        assert!(hub.reachable_from(alpha, true).is_empty());
        assert_eq!(
            hub.reachable_from(alpha, false).len(),
            1,
            "explicit addressing does not self-exclude",
        );
    }

    #[test]
    fn two_readers_of_one_channel_cannot_reach_each_other() {
        let hub = Hub::new();
        let (left, _left_rx) = join(&hub, "left", &[(7, Direction::Out)]);
        let (_right, _right_rx) = join(&hub, "right", &[(7, Direction::Out)]);

        assert!(hub.reachable_from(left, true).is_empty());
    }

    #[test]
    fn a_connection_is_indexed_by_the_callsign_and_uid_it_announces() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (bravo, _bravo_rx) = join(&hub, "bravo", &[(7, Direction::Both)]);
        identify(&hub, bravo, "UID-B", "BRAVO");

        assert_eq!(
            hub.resolve_callsigns(alpha, &["BRAVO".to_string()]).len(),
            1
        );
        assert_eq!(hub.resolve_uids(alpha, &["UID-B".to_string()]).len(), 1);
        assert!(
            hub.resolve_callsigns(alpha, &["NOBODY".to_string()])
                .is_empty()
        );
    }

    #[test]
    fn an_explicit_address_still_obeys_the_channel_rules() {
        // The gotcha in `compat/streaming.md` §8: naming a callsign is not a
        // way around a membership somebody does not hold.
        let hub = Hub::new();
        let (outsider, _outsider_rx) = join(&hub, "outsider", &[(9, Direction::Both)]);
        let (insider, _insider_rx) = join(&hub, "insider", &[(7, Direction::Both)]);
        identify(&hub, insider, "UID-I", "INSIDE");

        assert!(
            hub.resolve_callsigns(outsider, &["INSIDE".to_string()])
                .is_empty()
        );
    }

    #[test]
    fn an_address_list_naming_one_connection_twice_delivers_once() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (bravo, _bravo_rx) = join(&hub, "bravo", &[(7, Direction::Both)]);
        identify(&hub, bravo, "UID-B", "BRAVO");

        let handles = hub.resolve_callsigns(alpha, &["BRAVO".to_string(), "BRAVO".to_string()]);

        assert_eq!(handles.len(), 1);
    }

    #[test]
    fn unregistering_takes_the_connection_out_of_both_indexes() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (bravo, _bravo_rx) = join(&hub, "bravo", &[(7, Direction::Both)]);
        identify(&hub, bravo, "UID-B", "BRAVO");

        let gone = hub.unregister(bravo).expect("the subscription that left");

        assert_eq!(gone.callsign.as_deref(), Some("BRAVO"));
        assert!(
            hub.resolve_callsigns(alpha, &["BRAVO".to_string()])
                .is_empty()
        );
        assert!(hub.resolve_uids(alpha, &["UID-B".to_string()]).is_empty());
        assert_eq!(hub.len(), 1);
    }

    #[test]
    fn replay_carries_the_reachable_and_skips_the_invisible() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (ghost, _ghost_rx) = join(&hub, "ghost", &[(7, Direction::Both)]);
        let (stranger, _stranger_rx) = join(&hub, "stranger", &[(9, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");
        identify(&hub, ghost, "UID-G", "GHOST");
        identify(&hub, stranger, "UID-S", "STRANGE");
        hub.set_incognito(ghost, true);

        let (newcomer, _newcomer_rx) = join(&hub, "newcomer", &[(7, Direction::Both)]);
        let replay = hub.latest_sa_for(newcomer);

        assert_eq!(
            replay.len(),
            1,
            "alpha only: ghost hides, stranger cannot reach"
        );
        assert_eq!(replay[0].event().uid, "UID-A");
    }

    #[test]
    fn a_connection_that_never_spoke_has_nothing_to_replay() {
        let hub = Hub::new();
        let (_silent, _silent_rx) = join(&hub, "silent", &[(7, Direction::Both)]);
        let (newcomer, _newcomer_rx) = join(&hub, "newcomer", &[(7, Direction::Both)]);

        assert!(hub.latest_sa_for(newcomer).is_empty());
    }

    #[test]
    fn a_group_addressed_message_reaches_only_that_channels_readers() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both), (9, Direction::Both)]);
        let (_seven, _seven_rx) = join(&hub, "seven", &[(7, Direction::Out)]);
        let (_nine, _nine_rx) = join(&hub, "nine", &[(9, Direction::Out)]);

        assert_eq!(hub.reachable_in_group(alpha, 7).len(), 1);
        assert_eq!(hub.reachable_in_group(alpha, 9).len(), 1);
        assert!(hub.reachable_in_group(alpha, 11).is_empty());
    }

    #[test]
    fn a_revoked_certificate_finds_the_sessions_it_bought() {
        let hub = Hub::new();
        let (_alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);

        assert_eq!(
            hub.handles_for_fingerprint(&format!("{:f>64}", "alpha"))
                .len(),
            1
        );
        assert!(hub.handles_for_fingerprint(&"0".repeat(64)).is_empty());
    }

    #[test]
    fn the_snapshot_lists_only_the_clients_that_introduced_themselves() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (_quiet, _quiet_rx) = join(&hub, "quiet", &[(7, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");

        let snapshot = hub.snapshot();

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].callsign, "ALPHA");
        assert_eq!(snapshot[0].username, "alpha");
    }

    #[test]
    fn a_viewers_snapshot_is_narrowed_to_what_it_may_see() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (stranger, _stranger_rx) = join(&hub, "stranger", &[(9, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");
        identify(&hub, stranger, "UID-S", "STRANGE");

        let viewer = principal("viewer", &[(7, Direction::Out)]);

        let visible = hub.snapshot_for(&viewer);

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].callsign, "ALPHA");
    }

    #[test]
    fn identifiers_are_never_reused() {
        let hub = Hub::new();
        let first = hub.next_id();
        let second = hub.next_id();

        assert_ne!(first, second);
        assert!(hub.is_empty());
    }
}
