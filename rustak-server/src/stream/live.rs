//! [`LiveState`]: the handle to everything that is connected right now.
//!
//! The rest of the server does not want a [`Hub`], a [`Router`] and a
//! [`CotStoreHandle`]; it wants to answer three questions. *Who is connected?*
//! — `/Marti/api/clientEndPoints` and `/Marti/api/contacts/all`. *Tell this
//! person's devices something* — the channels API's `t-x-g-c`, the mission
//! store's invitations. *That certificate is revoked, close what it opened* —
//! the PKI hook. `LiveState` is those three, behind one cheap clone.
//!
//! # Why it is passed rather than reached for
//!
//! A process-wide singleton would be one line shorter and would make two test
//! servers in one process share a registry. The handle is built by the listener
//! and handed to whatever needs it, which is also what makes a listener a thing
//! a test can start, drive and drop.

use std::sync::Arc;

use rustak_cot::{CotTime, Event};

use crate::cot_store::CotStoreHandle;
use crate::prelude::*;

use super::hub::Hub;
use super::liveness::LeaveReason;
use super::metrics::StreamMetrics;
use super::notify::{self, Notifier};
use super::router::Router;
use super::subscription::{ClientEndpoint, ConnId, Subscription};

/// A connection joining or leaving, as a [`ConnectionWatcher`] hears about it.
///
/// Owned rather than a borrowed [`Subscription`], so that the hub can announce
/// a registration *after* it has taken ownership of one, and so that a watcher
/// cannot hold a connection alive by keeping what it was handed.
#[derive(Debug, Clone)]
pub enum ConnectionChange {
    /// A connection authenticated and was registered.
    Joined(ConnectionSummary),

    /// A connection was unregistered, whatever ended it.
    Left(ConnectionSummary),
}

impl ConnectionChange {
    /// The connection this is about.
    pub fn connection(&self) -> &ConnectionSummary {
        match self {
            Self::Joined(connection) | Self::Left(connection) => connection,
        }
    }
}

/// The little of a connection a watcher is told about.
#[derive(Debug, Clone)]
pub struct ConnectionSummary {
    /// Its identifier.
    pub id: ConnId,

    /// The account it authenticated as.
    pub username: Username,

    /// The uid it calls itself, once it has said — which is never, for a
    /// connection that joined and left without sending anything.
    pub client_uid: Option<String>,

    /// The callsign it reports, once it has said.
    pub callsign: Option<String>,

    /// Its effective group rights, so that a watcher can answer the same
    /// "who may see this" question the hub answers for the client listing.
    /// Shared rather than copied: this is the connection's own set.
    pub groups: Arc<GroupSet>,

    /// Whether the client has asked not to be shown to its peers. A watcher
    /// that announced it anyway would undo the feature over a different
    /// transport (R-01 M14).
    pub incognito: bool,
}

impl ConnectionSummary {
    /// What a registered connection looks like to a watcher.
    pub(super) fn of(subscription: &Subscription) -> Self {
        Self {
            id: subscription.id,
            username: subscription.principal.username.clone(),
            client_uid: subscription.client_uid.clone(),
            callsign: subscription.callsign.clone(),
            groups: Arc::clone(&subscription.principal.groups),
            incognito: subscription.incognito,
        }
    }
}

/// Something told about every connection that joins or leaves.
///
/// A newtype rather than a bare `Arc<dyn Fn…>` so that [`Hub`] keeps its
/// derived `Debug`: a closure has none, and a registry that could not be
/// printed would be a worse trade than one line of formatting here.
#[derive(Clone)]
pub struct ConnectionWatcher(pub(super) Arc<dyn Fn(&ConnectionChange) + Send + Sync>);

impl ConnectionWatcher {
    /// Wraps a function to be told about every change.
    pub fn new(watcher: impl Fn(&ConnectionChange) + Send + Sync + 'static) -> Self {
        Self(Arc::new(watcher))
    }
}

impl std::fmt::Debug for ConnectionWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionWatcher")
    }
}

/// How long a replay started from here waits for one connection's queue.
///
/// Replaced by `[stream.limits] write_timeout` where the listener builds the
/// state; this is what the two test constructions get.
const DEFAULT_REPLAY_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Everything that is connected, as the rest of the server sees it.
#[derive(Clone, Debug)]
pub struct LiveState {
    hub: Arc<Hub>,
    router: Arc<Router>,
    store: CotStoreHandle,
    metrics: Arc<StreamMetrics>,
    replay_budget: std::time::Duration,
    bound_at: chrono::DateTime<chrono::Utc>,
}

impl LiveState {
    /// Assembles the state from the pieces the listener builds.
    ///
    /// Built after the socket is bound and immediately before the registry is
    /// published, which is why [`bound_at`](Self::bound_at) is taken here: a
    /// listener that came up late — behind a certificate that had not been
    /// written yet, or after a restart the process itself did not have — makes
    /// "when did the server start" and "when did the stream start" different
    /// questions, and the client listing is where the second one is asked.
    pub fn new(
        hub: Arc<Hub>,
        router: Arc<Router>,
        store: CotStoreHandle,
        metrics: Arc<StreamMetrics>,
    ) -> Self {
        Self {
            bound_at: chrono::Utc::now(),
            hub,
            router,
            store,
            metrics,
            replay_budget: DEFAULT_REPLAY_BUDGET,
        }
    }

    /// Sets how long a replay started from here waits for one connection.
    #[must_use]
    pub fn with_replay_budget(mut self, budget: std::time::Duration) -> Self {
        self.replay_budget = budget;
        self
    }

    /// The registry itself, for code that genuinely needs it.
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The router, for an in-process producer that wants to inject CoT as if it
    /// had arrived on a connection.
    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    /// The store the stream records through.
    pub fn store(&self) -> &CotStoreHandle {
        &self.store
    }

    /// The listener's counters.
    pub fn metrics(&self) -> &Arc<StreamMetrics> {
        &self.metrics
    }

    /// When this registry was built, which is when its listener bound.
    ///
    /// Read by `GET /api/v1/clients/status`. A clone carries the original
    /// moment rather than the moment it was cloned — the handle is passed
    /// around, and every copy is the same listener.
    pub fn bound_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.bound_at
    }

    /// Tells the routing path that the channel table has changed.
    ///
    /// The name → bit position map `<dest group>` resolves against is cached,
    /// because reading it per destination element was a denial of service
    /// (R-03 H2). It refreshes itself within a second either way; this is for
    /// whatever creates, renames or deletes a channel and would rather not wait.
    pub fn channels_changed(&self) {
        self.router.channels_changed();
    }

    /// How many clients are connected.
    pub fn connected(&self) -> usize {
        self.hub.len()
    }

    /// Everyone connected, for the endpoint and contact listings.
    pub fn snapshot(&self) -> Vec<ClientEndpoint> {
        self.hub.snapshot()
    }

    /// Everyone a given reader is allowed to see.
    pub fn snapshot_for(&self, viewer: &Principal) -> Vec<ClientEndpoint> {
        self.hub.snapshot_for(viewer)
    }

    /// Pushing a server-originated message at connected clients.
    pub fn notifier(&self) -> Arc<dyn Notifier> {
        Arc::clone(&self.hub) as Arc<dyn Notifier>
    }

    /// Tells an account's other devices that its channels changed.
    ///
    /// `originating_uid` is the device whose own action caused the change, and
    /// is never told: the notice makes a client discard every map item this
    /// server gave it, which would undo what it had just done.
    pub fn groups_changed(&self, username: &Username, originating_uid: Option<&str>) -> usize {
        notify::on_groups_changed(
            &self.hub,
            username,
            originating_uid,
            uuid::Uuid::new_v4().to_string(),
            CotTime::now(),
        )
    }

    /// The connections an account has open, with the device each belongs to.
    ///
    /// The channels API reads one effective channel set per device — a database
    /// read — and then hands each one back through [`reauth`](Self::reauth).
    pub fn sessions_for_user(&self, username: &Username) -> Vec<(ConnId, Option<DeviceId>)> {
        self.hub.sessions_for_user(username)
    }

    /// Replaces one live connection's effective channels.
    ///
    /// What `PUT /Marti/api/groups/active` does to a device that is connected
    /// while its selection changes: without it the client's own idea of which
    /// channels it is on and the server's routing would disagree until it
    /// reconnected.
    pub fn reauth(&self, id: ConnId, groups: Arc<GroupSet>, names: Vec<GroupName>) -> bool {
        self.hub.reauth(id, groups, names)
    }

    /// Re-sends every peer's latest position to each of an account's devices.
    ///
    /// `GET /Marti/api/groups/all?sendLatestSA=true` is what ATAK calls after a
    /// `t-x-g-c`, having just thrown away every map item this server gave it —
    /// so the replay is how the map comes back, and it has to be computed
    /// *after* the connections have been re-authenticated or it would be the
    /// old channel selection's answer.
    ///
    /// Returns how many connections a replay was **started** for, not how many
    /// events it sent. The replay now applies backpressure rather than
    /// discarding what will not fit (R-03 C1), so it is a wait of up to
    /// `[stream.limits] write_timeout` per connection — and the caller is an
    /// HTTP handler answering the device that asked. It is spawned, because the
    /// map arriving is a side effect on the stream and the response has no
    /// reason to sit behind somebody else's slow radio.
    pub fn resend_latest_sa(&self, username: &Username) -> usize {
        let handles = self.hub.handles_for_user(username);
        let started = handles.len();

        if started == 0 {
            return 0;
        }

        let hub = Arc::clone(&self.hub);
        let metrics = Arc::clone(&self.metrics);
        let budget = self.replay_budget;

        tokio::spawn(async move {
            let mut sent = 0;

            for handle in handles {
                sent += super::replay::replay_latest_sa(&hub, handle.id(), budget, &metrics).await;
            }

            tracing::debug!(
                connections = started,
                events = sent,
                "Replayed the map after a channel change."
            );
        });

        started
    }

    /// Closes every connection a certificate authenticated.
    ///
    /// Registered with `pki.revocations().on_revoked(..)` at start-up, so that
    /// taking a certificate back ends the sessions it bought rather than only
    /// the ones it has not opened yet.
    pub fn disconnect_by_fingerprint(&self, fingerprint: &str) -> usize {
        self.hub.disconnect_by_fingerprint(fingerprint)
    }

    /// Closes every connection an account has open.
    ///
    /// The stream resolves a principal once, at the TLS handshake, and makes no
    /// database call thereafter — so switching an account off used to leave its
    /// device receiving every reachable peer's position and injecting CoT until
    /// its TCP connection happened to drop (R-01 H5). Disabling an account is
    /// the control an operator reaches for when a device is lost, so it has to
    /// end what is already open, not only refuse what comes next.
    ///
    /// Answers how many connections were closed.
    pub fn disconnect_by_user(&self, username: &Username) -> usize {
        let handles = self.hub.handles_for_user(username);

        for handle in &handles {
            handle.close(LeaveReason::AccountDisabled);
        }

        if !handles.is_empty() {
            info!(
                %username,
                connections = handles.len(),
                "Closed the stream connections of an account that may no longer use them."
            );
        }

        handles.len()
    }

    /// Sends one message to every connection claiming a `clientUid`.
    pub fn send_to_uid(&self, client_uid: &str, event: Event) -> usize {
        self.hub.send_to_uid(client_uid, event)
    }

    /// Sends one message to every connection an account has open.
    pub fn send_to_user(&self, username: &Username, event: Event) -> usize {
        self.hub.send_to_user(username, event)
    }

    /// Sends one message to one connection.
    pub fn send_to_conn(&self, id: ConnId, event: Event) -> bool {
        self.hub.send_to_conn(id, event)
    }

    /// Registers something to be told when a connection joins or leaves.
    ///
    /// This is the seam the server-event feed hangs off:
    /// [`AppContext::install_live`](crate::services::AppContext::install_live)
    /// registers one watcher, which turns every change into a `client.connected`
    /// or `client.disconnected` event on `GET /api/v1/events`. Nothing in this
    /// module knows what that feed is, which is the point of a watcher rather
    /// than a call.
    pub fn watch_connections(&self, watcher: ConnectionWatcher) {
        self.hub.watch(watcher);
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::Event;
    use tokio::sync::mpsc;

    use super::super::metrics::StreamMetrics;
    use super::super::mission_hook;
    use super::super::subscription::{ConnHandle, ConnStats, Outbound, Subscription};
    use super::*;

    async fn live() -> LiveState {
        let hub = Arc::new(Hub::new());
        let metrics = Arc::new(StreamMetrics::default());
        let store = CotStoreHandle::disabled();
        let router = Arc::new(Router::new(
            Arc::clone(&hub),
            crate::db::Database::open_in_memory().await.unwrap(),
            store.clone(),
            mission_hook::no_missions(),
            Arc::clone(&metrics),
            "rustak-test",
        ));

        LiveState::new(hub, router, store, metrics)
    }

    fn join(live: &LiveState, name: &str, uid: &str) -> mpsc::Receiver<Outbound> {
        let id = live.hub().next_id();
        let (tx, rx) = mpsc::channel(8);

        live.hub().register(Subscription::new(
            id,
            Arc::new(Principal::new(
                UserId::from(1),
                Username::parse(name).unwrap(),
                PrincipalKind::Person,
                AuthMethod::SetupToken,
            )),
            Vec::new(),
            format!("{name:f>64}"),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));

        let sa = Event::builder("a-f-G-U-C", uid)
            .point(51.5, -0.12)
            .typed(
                &rustak_cot::detail::Contact::new(name.to_uppercase()).with_endpoint("*:-1:stcp"),
            )
            .build();
        live.hub().apply_event(id, &sa, None);

        rx
    }

    #[tokio::test]
    async fn the_snapshot_is_what_a_contact_list_is_rendered_from() {
        let live = live().await;
        let _alpha = join(&live, "alice", "UID-ALICE");

        assert_eq!(live.connected(), 1);
        let snapshot = live.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].uid, "UID-ALICE");
        assert_eq!(snapshot[0].callsign, "ALICE");
    }

    #[tokio::test]
    async fn a_notice_reaches_the_account_it_names() {
        let live = live().await;
        let mut alpha = join(&live, "alice", "UID-ALICE");

        let sent = live.send_to_uid(
            "UID-ALICE",
            Event::builder("t-x-m-i", "n-1").point(0.0, 0.0).build(),
        );

        assert_eq!(sent, 1);
        assert!(matches!(alpha.try_recv(), Ok(Outbound::Event(_))));
    }

    #[tokio::test]
    async fn a_channel_change_reaches_the_users_devices() {
        let live = live().await;
        let mut alpha = join(&live, "alice", "UID-ALICE");

        let reached = live.groups_changed(&Username::parse("alice").unwrap(), None);

        assert_eq!(reached, 1);
        assert!(matches!(alpha.try_recv(), Ok(Outbound::Event(_))));
    }

    #[tokio::test]
    async fn a_revoked_certificate_closes_the_session_it_bought() {
        let live = live().await;
        let mut alpha = join(&live, "alice", "UID-ALICE");

        assert_eq!(
            live.disconnect_by_fingerprint(&format!("{:f>64}", "alice")),
            1
        );
        assert!(matches!(alpha.try_recv(), Ok(Outbound::Close)));
    }
}
