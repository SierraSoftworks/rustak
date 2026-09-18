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
use super::metrics::StreamMetrics;
use super::notify::{self, Notifier};
use super::router::Router;
use super::subscription::{ClientEndpoint, ConnId};

/// Everything that is connected, as the rest of the server sees it.
#[derive(Clone, Debug)]
pub struct LiveState {
    hub: Arc<Hub>,
    router: Arc<Router>,
    store: CotStoreHandle,
    metrics: Arc<StreamMetrics>,
}

impl LiveState {
    /// Assembles the state from the pieces the listener builds.
    pub fn new(
        hub: Arc<Hub>,
        router: Arc<Router>,
        store: CotStoreHandle,
        metrics: Arc<StreamMetrics>,
    ) -> Self {
        Self {
            hub,
            router,
            store,
            metrics,
        }
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

    /// Closes every connection a certificate authenticated.
    ///
    /// Registered with `pki.revocations().on_revoked(..)` at start-up, so that
    /// taking a certificate back ends the sessions it bought rather than only
    /// the ones it has not opened yet.
    pub fn disconnect_by_fingerprint(&self, fingerprint: &str) -> usize {
        self.hub.disconnect_by_fingerprint(fingerprint)
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
