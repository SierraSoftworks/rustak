//! [`AppContext`]: the one handle everything in the server is reached through.
//!
//! rustak is single-tenant, so the container automate parameterises by tenant
//! collapses to a single struct holding the configuration, the database, the
//! secret store, the content store, the telemetry session, a shared HTTP
//! client, the shutdown signal and the JWT signing keys. Cloning it is cheap —
//! every field is an [`Arc`] or an already-shared handle — so actix, the job
//! host and the stream listeners each take their own clone.
//!
//! # Why a trait as well as a struct
//!
//! [`Services`] is what request handlers, job handlers and domain modules take.
//! They are written against the trait so that a test can hand them something
//! else, and so that a signature says which capabilities the code it introduces
//! actually uses. [`AppContext`] is the one implementation that ships; the
//! second implementation is `&S`, so that a handler holding `&impl Services`
//! can pass it to a helper that takes ownership without cloning first.
//!
//! The trait's accessors return the *stores* rather than the database, because
//! `impl KeyValueStore + Clone + Send + Sync + 'static` is a narrower promise
//! than `&Database`: code that only reads key/value state cannot reach the
//! queue, the audit log or a raw SQL statement through it.
//!
//! # Handles that arrive late
//!
//! Two handles are not available when the context is built: the content store
//! and the JWT signing keys, both of which are set up after the database and
//! the secret store are open (the keys are *read from* the database, using the
//! secret store). They live in [`Late`] slots that start-up fills once, before
//! anything is spawned — see [`late`] for why, and
//! [`AppContext::install_content`]/[`AppContext::install_jwt`] for how.
//!
//! ```
//! use rustak_server::prelude::*;
//!
//! /// Written against the trait, so it says what it reaches for and a test can
//! /// hand it something else.
//! async fn remember_the_name(services: &impl Services) -> Result<(), human_errors::Error> {
//!     let name = services.config().server.name.clone();
//!
//!     services.kv().set("example", "name", name).await
//! }
//! ```
//!
//! Tests build one with `AppContext::new_mock`, which is compiled only under
//! `cfg(test)` or the `testing` feature.

pub mod late;
mod mock;
mod wiring;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rustak_core::{prelude::*, telemetry::Session};

use crate::{
    config::Config,
    crypto::SecretStore,
    db::{AuditStore, Cache, Database, KeyValueStore, Queue},
};

pub use late::{Late, Pending};

/// The content-addressed blob store.
///
/// Named here rather than reached for directly so that every caller sees one
/// type for "the blob store", whatever the module tree does with it later.
pub type ContentStore = crate::store::ContentStore;

/// The RS256 signing keys our own tokens are issued and verified with.
///
/// The keys are loaded from the `oauth_keys` table during start-up — after the
/// context exists, because loading them needs the database and the secret store
/// it carries — and installed with [`AppContext::install_jwt`].
pub type JwtKeys = crate::auth::jwt::JwtIssuer;

/// The internal certificate authority: enrolment, revocation and the
/// configurations the mutually authenticated listeners are built from.
///
/// Late for the same reason as the signing keys: loading it reads the database
/// through the secret store the context already carries, so it cannot exist
/// before the context does. Installed by
/// [`AppContext::install_pki`](AppContext::install_pki).
pub type PkiAuthority = crate::pki::Pki;

/// The registry of everything connected to the CoT stream right now.
///
/// Later than the authority: it is built by the stream listener, which is bound
/// after the context exists. Installed by
/// [`AppContext::install_live`](AppContext::install_live).
///
/// An installation with `[stream.tls] enabled = false` never installs one, and
/// [`AppContext::has_live`](AppContext::has_live) is how the contact and
/// client-endpoint endpoints answer an empty list rather than a `500`.
pub type LiveConnections = crate::stream::LiveState;

/// The `User-Agent` every outbound request carries.
///
/// Version included so that a server we talk to (an ACME directory, an identity
/// provider, a plugin's webhook endpoint) can tell which release is calling,
/// which is the difference between a bug report that can be acted on and one
/// that cannot.
pub const HTTP_USER_AGENT: &str = concat!("SierraSoftworks/rustak/", env!("CARGO_PKG_VERSION"));

/// Everything the server is, in one cloneable handle.
///
/// Built once during start-up and cloned into every listener, request handler
/// and job. See the [module documentation](self) for the shape and for the two
/// handles that are installed after construction.
#[derive(Clone)]
pub struct AppContext {
    config: Arc<Config>,
    db: Database,
    secrets: Arc<SecretStore>,
    content: Late<ContentStore>,
    jwt: Late<JwtKeys>,
    pki: Late<PkiAuthority>,
    live: Late<LiveConnections>,
    session: Arc<Session>,
    http_client: reqwest::Client,
    shutdown: Shutdown,
    started_at: DateTime<Utc>,
}

impl AppContext {
    /// Assembles the context from the handles start-up has opened.
    ///
    /// The content store and the signing keys are installed afterwards, through
    /// [`install_content`](Self::install_content) and
    /// [`install_jwt`](Self::install_jwt).
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error if the shared HTTP
    /// client cannot be built, which means the TLS backend failed to
    /// initialise — nothing that follows would work either.
    pub fn new(
        config: Config,
        db: Database,
        secrets: SecretStore,
        session: Arc<Session>,
        shutdown: Shutdown,
    ) -> Result<Self, Error> {
        let http_client = reqwest::Client::builder()
            .user_agent(HTTP_USER_AGENT)
            .build()
            .or_system_err(&[
                "This usually means the TLS backend could not be initialised.",
                "Please report this issue to the development team via GitHub.",
            ])?;

        Ok(Self {
            config: Arc::new(config),
            db,
            secrets: Arc::new(secrets),
            content: Late::new("the content store"),
            jwt: Late::new("the token signing keys"),
            pki: Late::new("the certificate authority"),
            live: Late::new("the live stream connections"),
            session,
            http_client,
            shutdown,
            started_at: Utc::now(),
        })
    }

    /// Installs the content store, once, during start-up.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error if it has already
    /// been installed.
    pub fn install_content(&self, content: Arc<ContentStore>) -> Result<(), Error> {
        self.content.install(content)
    }

    /// Installs the token signing keys, once, during start-up.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error if they have
    /// already been installed.
    pub fn install_jwt(&self, jwt: Arc<JwtKeys>) -> Result<(), Error> {
        self.jwt.install(jwt)
    }

    /// Installs the certificate authority, once, during start-up.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error if it has already
    /// been installed.
    pub fn install_pki(&self, pki: Arc<PkiAuthority>) -> Result<(), Error> {
        self.pki.install(pki)
    }

    /// The certificate authority, for the enrolment endpoints and the
    /// mutually authenticated listeners.
    ///
    /// An inherent method rather than a [`Services`] one: every caller holds an
    /// `AppContext` (the Marti handlers take `web::Data<AppContext>`), and
    /// widening the trait would make every hand-written stand-in implement a
    /// capability none of them can provide.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error when start-up has
    /// not installed it yet.
    pub fn pki(&self) -> Result<Arc<PkiAuthority>, Error> {
        self.pki.require()
    }

    /// Whether the certificate authority has been installed.
    ///
    /// Lets an endpoint answer "enrolment is not available on this
    /// installation" rather than a bare `500`.
    pub fn has_pki(&self) -> bool {
        self.pki.is_installed()
    }

    /// Installs the live stream registry, once, when the listener binds.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error if it has already
    /// been installed.
    pub fn install_live(&self, live: Arc<LiveConnections>) -> Result<(), Error> {
        self.live.install(live)
    }

    /// Everything connected to the CoT stream, for the contact, client-endpoint
    /// and subscription listings and for the `t-x-g-c` notices the channels API
    /// sends.
    ///
    /// An inherent method rather than a [`Services`] one, for the reason
    /// [`pki`](Self::pki) gives.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error when the stream
    /// listener is switched off or has not bound yet. Callers that can answer
    /// without it ask [`has_live`](Self::has_live) first.
    pub fn live(&self) -> Result<Arc<LiveConnections>, Error> {
        self.live.require()
    }

    /// Whether the stream listener has published its registry.
    ///
    /// `false` on an installation with `[stream.tls] enabled = false`, where
    /// nobody can be connected — so the listings answer an empty array rather
    /// than a failure.
    pub fn has_live(&self) -> bool {
        self.live.is_installed()
    }

    /// When this process finished starting up, for `/api/v1/health`'s uptime.
    pub fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
}

/// Written out because [`Session`] has no `Debug`, and because a context dump
/// in a log or a bug report must not carry the keys the secret store holds —
/// [`SecretStore`]'s own `Debug` already refuses to render them, and this keeps
/// the configuration's redactions in play too.
impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("db", &self.db)
            .field("secrets", &self.secrets)
            .field("content", &self.content)
            .field("jwt", &self.jwt)
            .field("pki", &self.pki)
            .field("live", &self.live)
            .field("shutdown", &self.shutdown)
            .field("started_at", &self.started_at)
            .finish_non_exhaustive()
    }
}

/// What the rest of the server is written against.
///
/// # Why there are no supertraits
///
/// The design sketch bounded this trait by `Send + Sync + 'static`, and all
/// three moved onto the use sites instead — as they are in automate, whose
/// handler signatures this keeps compatible with.
///
/// `'static` had to move: `&'a S` is never `'static`, so the borrowed
/// implementation below — the one that lets a handler pass its
/// `&impl Services` to a helper that takes ownership — could not otherwise
/// exist. `Send` and `Sync` followed it, because with them as supertraits
/// clippy's `implied_bounds_in_impls` refuses the
/// `impl Services + Send + Sync + 'static` that every job handler and spawned
/// task writes, and a signature that has to omit the bounds it depends on is
/// harder to read than one that states them.
pub trait Services {
    /// The parsed configuration file.
    fn config(&self) -> Arc<Config>;

    /// The telemetry session, for `record_event` and `record_human_error`.
    fn session(&self) -> &Session;

    /// Seals and opens the secrets we hold at rest.
    ///
    /// Reached through the services rather than a global so that a test can
    /// supply its own key, and so anything touching a secret says so in its
    /// signature.
    fn secrets(&self) -> &SecretStore;

    /// The database itself, for the repositories and for pragmas.
    ///
    /// Prefer [`kv`](Self::kv), [`queue`](Self::queue), [`cache`](Self::cache)
    /// and [`audit`](Self::audit) where one of them will do: they say what the
    /// caller reaches for, and they cannot reach anything else.
    fn db(&self) -> &Database;

    /// The content-addressed blob store.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error when start-up has
    /// not installed it yet.
    fn content(&self) -> Result<Arc<ContentStore>, Error>;

    /// The keys our own tokens are signed and verified with.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error when start-up has
    /// not installed them yet.
    fn jwt(&self) -> Result<Arc<JwtKeys>, Error>;

    /// The shared HTTP client, carrying [`HTTP_USER_AGENT`].
    ///
    /// Cloning one is cheap and shares the connection pool, so this is always
    /// preferable to building another.
    fn http_client(&self) -> reqwest::Client;

    /// The signal every long-running loop selects on.
    fn shutdown(&self) -> &Shutdown;

    /// Small, opaque state addressed by partition and key.
    fn kv(&self) -> impl KeyValueStore + Clone + Send + Sync + 'static;

    /// The work queue the job host consumes.
    fn queue(&self) -> impl Queue + Clone + Send + Sync + 'static;

    /// Read-through caching over the key/value store.
    fn cache(&self) -> impl Cache + Clone + Send + Sync + 'static;

    /// The audit log.
    fn audit(&self) -> impl AuditStore + Clone + Send + Sync + 'static;
}

impl Services for AppContext {
    fn config(&self) -> Arc<Config> {
        self.config.clone()
    }

    fn session(&self) -> &Session {
        &self.session
    }

    fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    fn db(&self) -> &Database {
        &self.db
    }

    fn content(&self) -> Result<Arc<ContentStore>, Error> {
        self.content.require()
    }

    fn jwt(&self) -> Result<Arc<JwtKeys>, Error> {
        self.jwt.require()
    }

    fn http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }

    fn shutdown(&self) -> &Shutdown {
        &self.shutdown
    }

    fn kv(&self) -> impl KeyValueStore + Clone + Send + Sync + 'static {
        self.db.clone()
    }

    fn queue(&self) -> impl Queue + Clone + Send + Sync + 'static {
        self.db.clone()
    }

    fn cache(&self) -> impl Cache + Clone + Send + Sync + 'static {
        self.db.clone()
    }

    fn audit(&self) -> impl AuditStore + Clone + Send + Sync + 'static {
        self.db.clone()
    }
}

/// Lets a borrowed handle stand in for an owned one.
///
/// Job handlers receive `&impl Services` from their context while the stores
/// built on top take ownership. Without this, every call site would have to
/// clone first — which says nothing useful and is easy to get wrong.
impl<S: Services + ?Sized> Services for &S {
    fn config(&self) -> Arc<Config> {
        (*self).config()
    }

    fn session(&self) -> &Session {
        (*self).session()
    }

    fn secrets(&self) -> &SecretStore {
        (*self).secrets()
    }

    fn db(&self) -> &Database {
        (*self).db()
    }

    fn content(&self) -> Result<Arc<ContentStore>, Error> {
        (*self).content()
    }

    fn jwt(&self) -> Result<Arc<JwtKeys>, Error> {
        (*self).jwt()
    }

    fn http_client(&self) -> reqwest::Client {
        (*self).http_client()
    }

    fn shutdown(&self) -> &Shutdown {
        (*self).shutdown()
    }

    fn kv(&self) -> impl KeyValueStore + Clone + Send + Sync + 'static {
        (*self).kv()
    }

    fn queue(&self) -> impl Queue + Clone + Send + Sync + 'static {
        (*self).queue()
    }

    fn cache(&self) -> impl Cache + Clone + Send + Sync + 'static {
        (*self).cache()
    }

    fn audit(&self) -> impl AuditStore + Clone + Send + Sync + 'static {
        (*self).audit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_context_is_built_from_the_handles_start_up_opens() {
        let context = AppContext::new_mock(|config| {
            config.server.name = "rustak-under-test".to_string();
        })
        .await
        .unwrap();

        assert_eq!(context.config().server.name, "rustak-under-test");
        assert!(!context.shutdown().is_cancelled());
        assert!(context.started_at() <= Utc::now());
        assert!(
            context
                .http_client()
                .get("https://example.invalid/")
                .build()
                .unwrap()
                .headers()
                .get("user-agent")
                .is_none(),
            "the agent is applied by the client, not written into each request",
        );
    }

    #[test]
    fn the_user_agent_names_the_release() {
        assert_eq!(
            HTTP_USER_AGENT,
            format!("SierraSoftworks/rustak/{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[tokio::test]
    async fn the_late_handles_report_themselves_missing_rather_than_panicking() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        // Until M0-10 lands these can never be installed, which is exactly what
        // a caller reaching for one before start-up filled it should see.
        assert!(context.content().is_err());
        assert!(context.jwt().is_err());
    }

    #[tokio::test]
    async fn the_stores_are_the_database_underneath() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        context.kv().set("test", "key", "value").await.unwrap();

        let read: Option<String> = context.kv().get("test", "key").await.unwrap();
        assert_eq!(read.as_deref(), Some("value"));
    }

    /// The borrowed implementation is what lets a handler pass its
    /// `&impl Services` to a helper that takes ownership.
    #[tokio::test]
    async fn a_borrowed_handle_stands_in_for_an_owned_one() {
        async fn takes_ownership(services: impl Services) -> String {
            services.config().server.name.clone()
        }

        let context = AppContext::new_mock(|config| {
            config.server.name = "borrowed".to_string();
        })
        .await
        .unwrap();

        assert_eq!(takes_ownership(&context).await, "borrowed");
    }

    #[tokio::test]
    async fn every_clone_shares_one_shutdown_signal() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let clone = context.clone();

        context.shutdown().cancel();

        assert!(clone.shutdown().is_cancelled());
    }
}
