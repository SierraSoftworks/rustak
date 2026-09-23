//! One connected client, as the hub sees it.
//!
//! A [`Subscription`] is the registry's record of a live connection: who it is,
//! what it has told us about itself, and the [`ConnHandle`] everything else
//! delivers through. The handle is deliberately separable from the record —
//! routing takes the lock, copies out the handles it needs and releases it
//! before anything is written, so a slow writer can never hold up the registry.
//!
//! # Identity comes from the wire, not from the certificate
//!
//! The certificate says which *account* a connection belongs to. The callsign,
//! the `clientUid`, the team and the platform all come out of the first
//! situational-awareness message the client sends, and are refreshed by every
//! one after it (`compat/streaming.md` §10). A client that never sends one is
//! connected, authenticated and invisible: nothing can address it by callsign
//! and its disconnect notice is not worth sending, because nobody ever saw it
//! arrive.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_cot::Event;
use rustak_cot::codec::{EncodedEvent, Mode};
use tokio::sync::mpsc;

use crate::prelude::*;

use super::liveness::{LeaveReason, Liveness};

/// What a subscription reports for a field the client has not sent.
pub const UNKNOWN: &str = "unknown";

/// A live connection's identifier, unique for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

impl std::fmt::Display for ConnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Something to write to one connection.
#[derive(Debug)]
pub enum Outbound {
    /// A message, shared with every other recipient of the same message.
    Event(Arc<EncodedEvent>),
    /// The negotiation answer, and the mode switch that must follow it on the
    /// same task so that no XML can be written after it.
    SwitchToProto(Arc<EncodedEvent>),
    /// Stop writing and close the socket.
    Close,
}

/// What became of a delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendResult {
    /// Queued for the writer.
    Sent,
    /// The queue was full; the message was dropped and the connection lives on.
    Dropped,
    /// The connection is gone, or has dropped so many in a row that it is being
    /// closed.
    Closed,
}

/// Per-connection counters, shared between the reader, the writer and routing.
#[derive(Debug, Default)]
pub struct ConnStats {
    /// Messages read and understood.
    pub rx_msgs: AtomicU64,
    /// Messages queued for the writer.
    pub tx_msgs: AtomicU64,
    /// Deliveries dropped for a full queue, ever.
    pub dropped: AtomicU64,
    /// Deliveries dropped for a full queue since the last one that fitted.
    pub dropped_consecutive: AtomicU64,
}

/// The delivery end of a connection.
///
/// Cloned out of the registry under the lock and used outside it. Cheap to
/// clone: an id, a sender and two `Arc`s.
#[derive(Clone, Debug)]
pub struct ConnHandle {
    id: ConnId,
    tx: mpsc::Sender<Outbound>,
    stats: Arc<ConnStats>,
    close_after_drops: u64,
    /// Cancelled to tear the connection down from outside it.
    ///
    /// A queue message would not do: the reason a connection is being closed
    /// from outside is usually that its queue is *full*, and the revocation
    /// hook must work on a connection that has stopped reading entirely.
    closing: Shutdown,
    /// The connection's clocks, and where [`close`](Self::close) writes the
    /// cause it was given.
    liveness: Arc<Liveness>,
}

impl ConnHandle {
    /// Builds a handle over a writer's queue.
    pub fn new(
        id: ConnId,
        tx: mpsc::Sender<Outbound>,
        stats: Arc<ConnStats>,
        close_after_drops: u64,
        closing: Shutdown,
    ) -> Self {
        Self {
            id,
            tx,
            stats,
            close_after_drops,
            closing,
            liveness: Arc::new(Liveness::new()),
        }
    }

    /// Shares the connection task's own [`Liveness`], so that closing this
    /// handle names the cause on the line that connection logs.
    #[must_use]
    pub fn with_liveness(mut self, liveness: Arc<Liveness>) -> Self {
        self.liveness = liveness;
        self
    }

    /// The connection's clocks and cause of death.
    pub fn liveness(&self) -> &Arc<Liveness> {
        &self.liveness
    }

    /// The token this connection's tasks stop on.
    pub fn closing(&self) -> &Shutdown {
        &self.closing
    }

    /// Which connection this delivers to.
    pub fn id(&self) -> ConnId {
        self.id
    }

    /// This connection's counters.
    pub fn stats(&self) -> &Arc<ConnStats> {
        &self.stats
    }

    /// Queues something for the writer without ever waiting.
    ///
    /// `try_send` rather than `send`, because the caller is the *sending*
    /// client's task: awaiting here would make one slow receiver stall the
    /// person who is talking. A queue that is full is a receiver that is behind,
    /// and [`SendResult::Closed`] after
    /// `close_after_drops` consecutive drops is what makes it reconnect and
    /// resynchronise rather than silently missing an arbitrary subset for ever.
    pub fn send(&self, outbound: Outbound) -> SendResult {
        match self.tx.try_send(outbound) {
            Ok(()) => {
                self.stats.tx_msgs.fetch_add(1, Ordering::Relaxed);
                self.stats.dropped_consecutive.store(0, Ordering::Relaxed);
                SendResult::Sent
            }
            Err(mpsc::error::TrySendError::Closed(_)) => SendResult::Closed,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                let run = self
                    .stats
                    .dropped_consecutive
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;

                if run >= self.close_after_drops {
                    self.close(LeaveReason::SlowConsumer);
                    SendResult::Closed
                } else {
                    SendResult::Dropped
                }
            }
        }
    }

    /// Queues a connect-time replay, waiting for room rather than discarding it.
    ///
    /// Two differences from [`send`](Self::send), and each one is a bug this
    /// method exists to fix (R-03 C1).
    ///
    /// It **waits**. The caller here is the *receiving* connection's own task,
    /// not somebody else's: a replay that awaits room applies backpressure to
    /// one new client instead of throwing away its map. The `try_send` loop
    /// this replaced pushed one event per connected peer with no await point
    /// between them, so above `queue_len` peers the rest were discarded and the
    /// client's map started out half empty.
    ///
    /// It is **exempt from the close counter**. Those discards were consecutive
    /// drops, so past `close_after_drops` of them the handle closed the
    /// connection the client had not yet read a byte from — turning a fleet
    /// restart above ~770 devices into a reconnect loop that looks like a
    /// network fault.
    ///
    /// `within` bounds the wait. A peer whose writer is not draining is one the
    /// idle timeout and `write_timeout` will reclaim; the replay does not wait
    /// on it.
    pub async fn send_replay(&self, outbound: Outbound, within: Duration) -> SendResult {
        match tokio::time::timeout(within, self.tx.send(outbound)).await {
            Ok(Ok(())) => {
                self.stats.tx_msgs.fetch_add(1, Ordering::Relaxed);
                SendResult::Sent
            }
            Ok(Err(_)) => SendResult::Closed,
            Err(_) => SendResult::Dropped,
        }
    }

    /// Tears the connection down, saying why.
    ///
    /// Both halves: the token so that a connection whose queue is full still
    /// stops, and the queue message so that a writer part-way through a drain
    /// finishes what it has and closes rather than being cut off mid-message.
    ///
    /// `reason` is what the connection's disconnect line will read, unless
    /// something got there first — see [`Liveness::ended`]. Every caller names
    /// one, because a close nobody can explain is the fault M9-15 was reported
    /// as.
    pub fn close(&self, reason: LeaveReason) {
        self.liveness.ended(reason);

        let _ = self.tx.try_send(Outbound::Close);
        self.closing.cancel();
    }

    /// Whether this connection has been told to stop.
    pub fn is_closing(&self) -> bool {
        self.closing.is_cancelled()
    }
}

impl SendResult {
    /// Whether the connection should be torn down.
    pub fn is_closed(self) -> bool {
        self == Self::Closed
    }
}

/// What reading an event told us about its sender.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaUpdate {
    /// The event fixed the subscription's `clientUid` and callsign for the
    /// first time, so the callsign and uid indexes need rebuilding.
    pub first_identity: bool,
    /// The event was a situational-awareness report and has been cached for
    /// replay to whoever connects next.
    pub is_sa: bool,
}

/// One registered connection.
#[derive(Debug)]
pub struct Subscription {
    /// Its identifier.
    pub id: ConnId,
    /// Who it is, and what it may reach.
    pub principal: Arc<Principal>,
    /// The channels it holds in the `OUT` direction, by name, for the contact
    /// and endpoint listings.
    pub groups: Vec<GroupName>,
    /// The sha256 of the certificate it authenticated with, so revoking that
    /// certificate can find it.
    pub fingerprint: String,
    /// The device row this connection belongs to, when its certificate named
    /// one — the foreign key `cot_latest` records against.
    pub device_id: Option<DeviceId>,
    /// Where it connected from.
    pub peer: SocketAddr,
    /// When it connected.
    pub connected_at: DateTime<Utc>,
    /// When it was last heard from.
    pub last_rx: DateTime<Utc>,
    /// The uid it calls itself, from the first message carrying a contact
    /// endpoint.
    pub client_uid: Option<String>,
    /// The callsign it reports.
    pub callsign: Option<String>,
    /// Its team colour, from `<__group name>`.
    pub team: String,
    /// Its role, from `<__group role>`.
    pub role: String,
    /// `platform:version`, from `<takv>`.
    pub takv: String,
    /// Its most recent situational-awareness message, replayed to whoever
    /// connects next.
    pub latest_sa: Option<Arc<EncodedEvent>>,
    /// The type of that message, which the disconnect notice echoes.
    pub last_sa_type: Option<String>,
    /// Whether it has asked to be invisible except to explicit addressing.
    pub incognito: bool,
    /// Which encoding it is being written in.
    pub mode: Mode,
    /// Where to deliver.
    pub handle: ConnHandle,
    /// Whether this is a stand-in for a sender that has no connection — a
    /// message published from the admin console — registered for one message
    /// and gone. Nobody is told it joined or left, because nothing did.
    pub ephemeral: bool,
}

impl Subscription {
    /// Registers a connection that has authenticated but said nothing yet.
    pub fn new(
        id: ConnId,
        principal: Arc<Principal>,
        groups: Vec<GroupName>,
        fingerprint: String,
        peer: SocketAddr,
        handle: ConnHandle,
    ) -> Self {
        let now = Utc::now();

        Self {
            id,
            principal,
            groups,
            fingerprint,
            peer,
            connected_at: now,
            last_rx: now,
            client_uid: None,
            callsign: None,
            team: UNKNOWN.to_string(),
            role: UNKNOWN.to_string(),
            takv: UNKNOWN.to_string(),
            latest_sa: None,
            last_sa_type: None,
            incognito: false,
            mode: Mode::Xml,
            handle: handle.clone(),
            device_id: None,
            ephemeral: false,
        }
    }

    /// Marks the subscription as standing in for a sender with no connection.
    #[must_use]
    pub fn ephemeral(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    /// Names the device row this connection's certificate was issued to.
    #[must_use]
    pub fn with_device_id(mut self, device_id: Option<DeviceId>) -> Self {
        self.device_id = device_id;
        self
    }

    /// Starts the subscription already invisible, because the device asked to
    /// be last time it was here.
    #[must_use]
    pub fn with_incognito(mut self, incognito: bool) -> Self {
        self.incognito = incognito;
        self
    }

    /// Folds an inbound event into what we know about the sender.
    ///
    /// The first event carrying a contact endpoint fixes the identity; every
    /// situational-awareness message after it refreshes the descriptive fields
    /// and replaces the cached replay copy. A field the client did not send
    /// leaves the previous value alone — ATAK, WinTAK, iTAK and CloudTAK each
    /// send a different subset.
    pub fn apply_event(&mut self, event: &Event, encoded: Option<&Arc<EncodedEvent>>) -> SaUpdate {
        self.last_rx = Utc::now();

        let Some(callsign) = event.callsign().filter(|name| !name.is_empty()) else {
            return SaUpdate::default();
        };

        if event.endpoint().is_none_or(str::is_empty) {
            return SaUpdate::default();
        }

        let first_identity = self.client_uid.is_none();
        if first_identity {
            self.client_uid = Some(event.uid.clone());
        }

        self.callsign = Some(callsign.to_string());

        if let Some(group) = event.group() {
            self.team = non_empty(group.name).unwrap_or_else(|| UNKNOWN.to_string());
            self.role = non_empty(group.role).unwrap_or_else(|| UNKNOWN.to_string());
        }

        if let Some(takv) = event.takv() {
            // `summary()` is `platform:version`, which is a bare colon when the
            // client sent a `<takv>` with neither — worse than saying nothing.
            let summary = takv.summary();
            if summary.len() > 1 {
                self.takv = summary;
            }
        }

        // Only the client's *own* report is worth replaying: a message it
        // relayed on somebody else's behalf would put a stale position back on
        // the map under the wrong uid.
        let is_sa = self.client_uid.as_deref() == Some(event.uid.as_str());
        if is_sa {
            self.last_sa_type = Some(event.r#type.clone());
            if let Some(encoded) = encoded {
                self.latest_sa = Some(Arc::clone(encoded));
            }
        }

        SaUpdate {
            first_identity,
            is_sa,
        }
    }

    /// Whether this subscription has said enough about itself to be listed.
    pub fn is_identified(&self) -> bool {
        self.client_uid.is_some() && self.callsign.is_some()
    }
}

/// `None` for a string the client sent as empty.
fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// One connected client, as `/Marti/api/clientEndPoints` and
/// `/Marti/api/contacts/all` describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEndpoint {
    /// The uid the client calls itself.
    pub uid: String,
    /// Its callsign.
    pub callsign: String,
    /// The account it authenticated as.
    pub username: String,
    /// Its team colour.
    pub team: String,
    /// Its role.
    pub role: String,
    /// `platform:version`.
    pub takv: String,
    /// The channels it receives from.
    pub groups: Vec<GroupName>,
    /// When it was last heard from.
    pub last_status: DateTime<Utc>,
    /// When it connected.
    pub connected_at: DateTime<Utc>,
    /// Whether it is invisible to implicit broadcast.
    pub incognito: bool,
    /// Which encoding it is being written in.
    pub mode: Mode,
    /// Where it connected from.
    pub peer: SocketAddr,
}

impl Subscription {
    /// The listing form, for a subscription that has identified itself.
    pub fn endpoint(&self) -> Option<ClientEndpoint> {
        Some(ClientEndpoint {
            uid: self.client_uid.clone()?,
            callsign: self.callsign.clone()?,
            username: self.principal.username.to_string(),
            team: self.team.clone(),
            role: self.role.clone(),
            takv: self.takv.clone(),
            groups: self.groups.clone(),
            last_status: self.last_rx,
            connected_at: self.connected_at,
            incognito: self.incognito,
            mode: self.mode,
            peer: self.peer,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustak_cot::detail::{Contact, Group, Takv, contact::STREAMING_ENDPOINT};

    use super::*;

    fn principal() -> Arc<Principal> {
        Arc::new(Principal::new(
            UserId::from(1),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::ClientCert {
                fingerprint: "ab".repeat(32),
                serial: "0f".repeat(8),
            },
        ))
    }

    fn subscription(queue: usize) -> (Subscription, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(queue);
        let handle = ConnHandle::new(
            ConnId(1),
            tx,
            Arc::new(ConnStats::default()),
            2,
            Shutdown::new(),
        );

        (
            Subscription::new(
                ConnId(1),
                principal(),
                Vec::new(),
                "ab".repeat(32),
                "127.0.0.1:9000".parse().unwrap(),
                handle,
            ),
            rx,
        )
    }

    fn sa(uid: &str, callsign: &str) -> Event {
        Event::builder("a-f-G-U-C", uid)
            .how("m-g")
            .point(51.5, -0.12)
            .stale_after(Duration::from_secs(60))
            .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
            .typed(&Group::new("Cyan", "Team Member"))
            .typed(&Takv {
                device: "Pixel".into(),
                platform: "ATAK-CIV".into(),
                os: "34".into(),
                version: "5.2".into(),
                extra: Vec::new(),
            })
            .build()
    }

    #[test]
    fn the_first_message_with_a_contact_endpoint_fixes_the_identity() {
        let (mut subscription, _rx) = subscription(4);

        let update = subscription.apply_event(&sa("UID-A", "ALPHA"), None);

        assert!(update.first_identity);
        assert!(update.is_sa);
        assert_eq!(subscription.client_uid.as_deref(), Some("UID-A"));
        assert_eq!(subscription.callsign.as_deref(), Some("ALPHA"));
        assert_eq!(subscription.team, "Cyan");
        assert_eq!(subscription.takv, "ATAK-CIV:5.2");
    }

    #[test]
    fn a_later_message_refreshes_without_re_fixing_the_uid() {
        let (mut subscription, _rx) = subscription(4);
        subscription.apply_event(&sa("UID-A", "ALPHA"), None);

        let update = subscription.apply_event(&sa("UID-A", "ALPHA-2"), None);

        assert!(!update.first_identity, "the uid is fixed once");
        assert_eq!(subscription.callsign.as_deref(), Some("ALPHA-2"));
    }

    #[test]
    fn a_relayed_message_under_somebody_elses_uid_is_not_this_clients_position() {
        // A bridge forwarding a peer's SA would otherwise overwrite its own
        // replay copy with somebody else's position.
        let (mut subscription, _rx) = subscription(4);
        subscription.apply_event(&sa("UID-A", "ALPHA"), None);

        let update = subscription.apply_event(&sa("UID-B", "BRAVO"), None);

        assert!(!update.is_sa);
        assert_eq!(subscription.client_uid.as_deref(), Some("UID-A"));
    }

    #[test]
    fn a_message_without_a_contact_endpoint_says_nothing_about_identity() {
        let (mut subscription, _rx) = subscription(4);
        let chat = Event::builder("b-t-f", "UID-CHAT")
            .point(0.0, 0.0)
            .typed(&Contact::new("ALPHA"))
            .build();

        let update = subscription.apply_event(&chat, None);

        assert_eq!(update, SaUpdate::default());
        assert!(!subscription.is_identified());
    }

    #[test]
    fn a_full_queue_drops_rather_than_waiting() {
        // The property the whole design rests on: the *sender's* task must
        // never be held up by a receiver that is behind.
        let (subscription, _rx) = subscription(1);
        let event = Arc::new(EncodedEvent::new(sa("UID-A", "ALPHA")));

        assert_eq!(
            subscription
                .handle
                .send(Outbound::Event(Arc::clone(&event))),
            SendResult::Sent
        );
        assert_eq!(
            subscription
                .handle
                .send(Outbound::Event(Arc::clone(&event))),
            SendResult::Dropped
        );
    }

    #[test]
    fn a_connection_that_keeps_dropping_is_closed() {
        let (subscription, _rx) = subscription(1);
        let event = Arc::new(EncodedEvent::new(sa("UID-A", "ALPHA")));

        subscription
            .handle
            .send(Outbound::Event(Arc::clone(&event)));
        subscription
            .handle
            .send(Outbound::Event(Arc::clone(&event)));

        assert!(
            subscription.handle.send(Outbound::Event(event)).is_closed(),
            "two consecutive drops reaches the threshold this handle was built with",
        );
    }

    #[test]
    fn a_delivery_that_fits_forgives_the_drops_before_it() {
        let (subscription, mut rx) = subscription(1);
        let event = Arc::new(EncodedEvent::new(sa("UID-A", "ALPHA")));

        subscription
            .handle
            .send(Outbound::Event(Arc::clone(&event)));
        subscription
            .handle
            .send(Outbound::Event(Arc::clone(&event)));
        rx.try_recv().expect("the queued message");

        assert_eq!(
            subscription.handle.send(Outbound::Event(event)),
            SendResult::Sent
        );
        assert_eq!(
            subscription
                .handle
                .stats()
                .dropped_consecutive
                .load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn a_closed_connection_reports_itself_closed() {
        let (subscription, rx) = subscription(1);
        drop(rx);

        assert!(subscription.handle.send(Outbound::Close).is_closed());
    }

    #[test]
    fn only_an_identified_subscription_is_listed() {
        let (mut subscription, _rx) = subscription(4);

        assert!(subscription.endpoint().is_none());

        subscription.apply_event(&sa("UID-A", "ALPHA"), None);
        let endpoint = subscription.endpoint().expect("an identified subscription");

        assert_eq!(endpoint.uid, "UID-A");
        assert_eq!(endpoint.callsign, "ALPHA");
        assert_eq!(endpoint.username, "alice");
        assert_eq!(endpoint.mode, Mode::Xml);
    }
}
