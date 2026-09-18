//! The CoT stream listener on `:8089`.
//!
//! This is the half of rustak that TAK clients spend their time talking to: one
//! mutually authenticated TLS socket per device, XML or TAK Protocol v1
//! framing, and a fan-out that decides for every message which of the other
//! connections is allowed to see it. `compat/streaming.md` is the contract; the
//! module tree follows the shape of it.
//!
//! | File | What it is |
//! |---|---|
//! | [`listener_tls`] | the socket, the handshake, the connection limit |
//! | [`resolver`] | a verified certificate → a [`Principal`] |
//! | [`connection`] | one connection's life, in the order the contract fixes |
//! | [`writer`] | the task that writes, and the 64 KiB substitution |
//! | [`negotiation`] | the one `t-x-takp-v` and what may follow it |
//! | [`hub`] | the lock, and every routing question asked under it |
//! | [`registry`] | the map of connections and the two indexes over it |
//! | [`router`] | flow tags, `<marti>` stripping, incognito, fan-out |
//! | [`dest`] | `<dest>` → a list of connections |
//! | [`control`] | the types consumed here and never relayed |
//! | [`replay`] | the latest position of everyone a newcomer may see |
//! | [`notify`] | `t-x-d-d`, `t-x-g-c`, and closing a revoked session |
//! | [`mission_hook`] | where `<dest mission>` will go in M4 |
//! | [`mission_notify`] | the `t-x-m-*` notices a mission pushes |
//! | [`mission_payload`] | what those notices carry inside `<mission>` |
//! | [`live`] | the handle the rest of the server holds |
//! | [`metrics`] | what the listener counts |
//!
//! # There is no plaintext listener
//!
//! TAK Server offers one, with a `<auth>` element carrying a password in the
//! clear and an anonymous mode that carries nothing at all. rustak has neither:
//! `[stream.tls]` is the only section, a client certificate is always required,
//! and `[stream.tcp]` is refused by name so that somebody porting a
//! configuration is told rather than left believing 8087 is open.
//!
//! # Reachability, once
//!
//! Every routing decision in this module reduces to one question, and it is not
//! symmetric and not a set intersection:
//!
//! > `S` may reach `R` **iff** there is a channel `G` that `S` holds `IN` and
//! > `R` holds `OUT`.
//!
//! It is asked per (sender, candidate) pair, per message, and it is never
//! cached — a membership changes and the next message is routed by the new one.

pub mod connection;
pub mod control;
pub mod dest;
pub mod hub;
pub mod listener_tls;
pub mod live;
pub mod metrics;
pub mod mission_hook;
pub mod mission_notify;
pub mod mission_payload;
pub mod negotiation;
pub mod notify;
pub mod registry;
pub mod replay;
pub mod resolver;
pub mod router;
pub mod subscription;
pub mod writer;

use std::sync::Arc;

use crate::config::stream::NegotiationMode;
use crate::cot_store::{self, CotStoreHandle, CotStoreOptions};
use crate::pki::Pki;
use crate::prelude::*;

pub use connection::{ConnDeps, ConnLimits};
pub use dest::{DropReason, Selection};
pub use hub::Hub;
pub use live::LiveState;
pub use metrics::StreamMetrics;
pub use mission_hook::{MissionIngest, MissionRef, NoMissions};
pub use mission_notify::{ChangeKind, MissionNotice, NoticeMission, Recipients};
pub use mission_payload::{
    MissionChangeXml, MissionLayerXml, MissionRoleXml, ResourceXml, UidDetailsXml,
};
pub use notify::Notifier;
pub use resolver::{CertPrincipalResolver, DbPrincipalResolver, StreamPrincipal};
pub use router::{Disposition, Router};
pub use subscription::{ClientEndpoint, ConnHandle, ConnId, Subscription};

/// A bound stream listener, before it has started accepting.
///
/// Built in two steps so that a test can learn the port before anything
/// connects, and so that a listener that will not bind fails start-up rather
/// than being discovered by the first device that cannot reach it.
pub struct StreamRuntime {
    live: LiveState,
    bound: listener_tls::Bound,
    tls: Arc<rustls::ServerConfig>,
    resolver: Arc<dyn CertPrincipalResolver>,
    deps: ConnDeps,
    store: tokio::task::JoinHandle<()>,
    handshake_timeout: std::time::Duration,
    max_connections: usize,
    drain: std::time::Duration,
}

impl std::fmt::Debug for StreamRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamRuntime")
            .field("address", &self.bound.local_addr())
            .field("max_connections", &self.max_connections)
            .finish_non_exhaustive()
    }
}

impl StreamRuntime {
    /// Binds the listener and starts the store task behind it.
    ///
    /// The revocation hook is registered here: a certificate taken back while a
    /// device is connected has to close that connection, and the only moment
    /// both the authority and the registry exist is this one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the address will not bind, and
    /// whatever [`Pki::stream_server_config`] reports when the TLS
    /// configuration cannot be built.
    pub async fn bind(
        context: &AppContext,
        pki: &Arc<Pki>,
        missions: Arc<dyn MissionIngest>,
    ) -> Result<Self, Error> {
        let config = context.config();
        let limits = &config.stream.limits;

        let bound = listener_tls::bind(&config.stream.tls.listen).await?;
        let tls = Arc::new(pki.stream_server_config()?);

        let (store, store_task) = if limits.record_history {
            let (handle, task) = cot_store::start(
                context.db().clone(),
                config.streams_dir(),
                CotStoreOptions::default(),
                context.shutdown().child(),
            );

            (handle, task)
        } else {
            info!("CoT recording is off; nothing relayed will be readable back.");

            (
                CotStoreHandle::disabled(),
                tokio::spawn(std::future::ready(())),
            )
        };

        let hub = Arc::new(Hub::new());
        let metrics = Arc::new(StreamMetrics::default());
        let router = Arc::new(Router::new(
            Arc::clone(&hub),
            context.db().clone(),
            store.clone(),
            missions,
            Arc::clone(&metrics),
            config.server.name.clone(),
        ));

        let live = LiveState::new(
            Arc::clone(&hub),
            Arc::clone(&router),
            store,
            Arc::clone(&metrics),
        );

        let closing = live.clone();
        pki.revocations()
            .on_revoked(Arc::new(move |fingerprint: &str| {
                closing.disconnect_by_fingerprint(fingerprint);
            }));

        let deps = ConnDeps {
            router,
            metrics,
            server_version: server_version(&config),
            public_url: config.server.base_url(),
            limits: ConnLimits {
                max_frame: limits.max_frame,
                queue_len: limits.queue_len,
                close_after_drops: limits.close_after_drops,
                idle_timeout: to_std(config.stream.tls.idle_timeout),
                // `negotiate_protobuf = false` is the operational way to turn
                // the offer off; `[stream] negotiation` is the compatibility
                // switch, and it never overrides the operational one.
                negotiate: if limits.negotiate_protobuf {
                    config.stream.negotiation
                } else {
                    NegotiationMode::Silent
                },
            },
        };

        Ok(Self {
            live,
            bound,
            tls,
            resolver: Arc::new(DbPrincipalResolver::new(
                context.db().clone(),
                config.auth.anon_group_default,
            )),
            deps,
            store: store_task,
            handshake_timeout: to_std(limits.handshake_timeout),
            max_connections: limits.max_connections,
            drain: config.server.shutdown_budget(),
        })
    }

    /// The address the listener bound.
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.bound.local_addr()
    }

    /// The handle the rest of the server holds.
    pub fn live(&self) -> &LiveState {
        &self.live
    }

    /// Replaces the resolver, for a test that authenticates its own way.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn CertPrincipalResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Accepts connections until `shutdown` is cancelled, then drains.
    ///
    /// Both waits — for the connections and then for the store task behind them
    /// — are bounded by `[server] shutdown_timeout`, the same budget the HTTP
    /// listeners drain within. It used to be a fixed ten seconds here, which is
    /// `docker stop`'s whole grace period on its own.
    ///
    /// # Errors
    ///
    /// Whatever [`listener_tls::run`] reports, which is nothing an accept loop
    /// can recover from on its own.
    pub async fn run(self, shutdown: Shutdown) -> Result<(), Error> {
        let limits = listener_tls::ListenerLimits {
            handshake_timeout: self.handshake_timeout,
            max_connections: self.max_connections,
            drain: self.drain,
        };

        let outcome = listener_tls::run(
            self.bound,
            self.tls,
            self.resolver,
            self.deps,
            limits,
            shutdown,
        )
        .await;

        // The store task stops on its own child token; waiting for it here is
        // what makes "the listener has stopped" mean the history it relayed has
        // been written.
        let _ = tokio::time::timeout(self.drain, self.store).await;

        outcome
    }
}

/// Binds and runs the stream listener, for [`runtime::run_all`].
///
/// `missions` is the `<dest mission>` publisher, passed in rather than built
/// here so that this module never has to know what a mission is — the M1 stub
/// ([`mission_hook::no_missions`]) and the real one satisfy the same trait.
///
/// `pki` is [`None`] on an installation that has turned the listener off,
/// because loading the authority also issues this server's own certificate and
/// a development server with no configured host name has none to issue.
///
/// An installation with no listener still waits here rather than returning:
/// every component in the runtime is joined, and one that finished early would
/// stop the server.
///
/// [`runtime::run_all`]: crate::runtime::run_all
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the address will not bind.
pub async fn serve(
    context: AppContext,
    pki: Option<Arc<Pki>>,
    missions: Arc<dyn MissionIngest>,
) -> Result<(), Error> {
    let shutdown = context.shutdown().clone();

    let Some(pki) = pki.filter(|_| context.config().stream.tls.enabled) else {
        info!("The CoT stream listener is turned off.");
        shutdown.cancelled().await;

        return Ok(());
    };

    let runtime = StreamRuntime::bind(&context, &pki, missions).await?;

    // Published the moment the registry exists, because the Marti surface reads
    // it: `/Marti/api/contacts/all`, `/Marti/api/clientEndPoints` and the
    // `t-x-g-c` the channels API sends all go through `AppContext::live`. An
    // installation with the listener switched off returns above without
    // installing anything, and those endpoints answer an empty list.
    context.install_live(Arc::new(runtime.live().clone()))?;

    runtime.run(shutdown).await
}

/// What this server calls itself in a `t-x-takp-v` offer.
///
/// CloudTAK stores it and shows it to an operator, so it is the release rather
/// than the installation's own name — which is what somebody comparing a client
/// against a server actually needs.
fn server_version(config: &crate::config::Config) -> String {
    let _ = config;

    format!("rustak-{}", env!("CARGO_PKG_VERSION"))
}

/// A configured duration as the standard library spells one.
///
/// A negative value cannot be a timeout, and refusing to start over one an
/// operator typed a minus sign into would be disproportionate — so it falls
/// back to a minute, which is long enough never to be the surprising part.
fn to_std(duration: chrono::Duration) -> std::time::Duration {
    duration
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60))
}
