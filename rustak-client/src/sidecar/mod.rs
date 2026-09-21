//! What a plugin is, and what the harness does with one.
//!
//! A rustak plugin is not loaded into the server: it is its own process, with
//! its own identity, that connects to the server the way an ATAK device does.
//! That is the whole of the plugin model — there is no plugin ABI to version,
//! no dynamic loading, and nothing a plugin can do that a well-behaved client
//! could not. What this module adds is the boilerplate every such process would
//! otherwise write for itself: a command line, a configuration file, telemetry,
//! a shutdown signal, and a loop.
//!
//! A plugin therefore consists of a type implementing [`Sidecar`] and a `main`
//! that hands it to [`run()`](run()):
//!
//! ```no_run
//! use rustak_client::sidecar::{NoSettings, Sidecar, SidecarContext, async_trait, run};
//! use rustak_core::prelude::*;
//!
//! #[derive(Default)]
//! struct Beacon;
//!
//! #[async_trait]
//! impl Sidecar for Beacon {
//!     const NAME: &'static str = env!("CARGO_PKG_NAME");
//!     const VERSION: &'static str = env!("CARGO_PKG_VERSION");
//!     type Settings = NoSettings;
//!
//!     async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
//!         info!(service = %ctx.identity().name(), "Up.");
//!         Ok(())
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     run::<Beacon>().await;
//! }
//! ```
//!
//! # What the harness wires up, and what it does not
//!
//! The harness loads the configuration, brings telemetry up, builds the
//! [`ServiceIdentity`] and the [`ServiceDescriptor`] the plugin will register
//! with, opens the CoT stream that `[server] stream` names, calls
//! [`Sidecar::start`], calls [`Sidecar::tick`] on an interval, asks
//! [`Sidecar::health`] what to report after each one, delivers everything the
//! stream produces to [`Sidecar::on_event`], and calls [`Sidecar::stop`] when
//! the process is asked to stop.
//!
//! Publishing runs the other way: [`tick`](Sidecar::tick) and
//! [`on_event`](Sidecar::on_event) *return* the [`Event`]s they want written,
//! and the harness writes them. A plugin therefore never holds a socket, and
//! never has to decide what to do about one that is reconnecting.
//!
//! The Marti client lands in M2 and the `/api/v1/services/*` control client in
//! M6; the shape they will arrive through is already here.
//! [`SidecarEvent`] is `#[non_exhaustive]`: match it with a `_` arm and new
//! variants are an additive change rather than a broken build.
//!
//! See `docs/plugins.md` for the operator-facing version of all of this.

pub mod config;
mod control_link;
pub(crate) mod enrolment;
mod link;
mod link_health;
pub mod run;
pub mod workload;

use std::sync::Arc;

use rustak_api::Heartbeat;
use rustak_core::prelude::*;
use rustak_core::service::{ServiceDescriptor, ServiceIdentity};
use rustak_cot::Event;
use tracing::Span;

use crate::control::{ControlClient, ServerEvent};
use crate::marti::MartiClient;

pub use async_trait::async_trait;
pub use config::{
    ENROLLMENT_TOKEN_ENV, HarnessConfig, NoSettings, ServerConfig, ServiceConfig, SidecarConfig,
};
pub use run::{Args, drive, run, run_with, serve};
pub use workload::{
    AccessTokens, KUBERNETES_TOKEN_PATH, NOMAD_TOKEN_ENV, Source, WorkloadIdentity,
};

pub(crate) use control_link::ControlLink;
pub(crate) use link::Link;

/// Something the sidecar harness noticed that a plugin may want to react to.
///
/// Every variant is delivered to [`Sidecar::on_event`]. The enum is
/// `#[non_exhaustive]`, so a plugin matches it with a `_` arm and keeps
/// compiling as the Marti and control clients land in later milestones.
///
/// Control traffic — the keepalive ping and pong, and the `t-x-takp-*`
/// negotiation exchange — never appears here. It is answered inside
/// [`TakStream`](crate::stream::TakStream), because a plugin has no use for a
/// pong and every plugin would otherwise have to remember to ignore one.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SidecarEvent {
    /// The CoT stream connection came up, against the endpoint named.
    ///
    /// A reconnected client is a *new* subscription as far as the server is
    /// concerned — no callsign, no latest position — so this is where a plugin
    /// returns the situational-awareness event that says who it is.
    Connected {
        /// The connect string that was dialled, e.g. `ssl://tak.example.com:8089`.
        endpoint: String,
    },

    /// The connection settled on an encoding, once per connection and always
    /// after [`Connected`](Self::Connected).
    ///
    /// `protobuf` is false for a server that does not offer TAK Protocol v1,
    /// refuses the request, or never answers it — all of which leave the
    /// connection speaking XML and working normally.
    Negotiated {
        /// Whether both directions switched to TAK Protocol v1.
        protobuf: bool,
    },

    /// The CoT stream connection dropped. The harness reconnects with backoff;
    /// this is for a plugin that wants to count outages or pause its own work.
    Disconnected {
        /// Why the connection ended, for logging rather than for matching on.
        reason: String,
    },

    /// An event arrived on the CoT stream, already decoded from whichever
    /// encoding the connection settled on.
    ///
    /// Boxed because a CoT event is an order of magnitude larger than the other
    /// variants and a plugin passes events around by value.
    Cot(Box<Event>),

    /// Something happened on the server that a plugin may want to react to: a
    /// device joined the stream, a mission changed, a package arrived.
    ///
    /// Delivered only when `[server] control` is set, from
    /// `GET /api/v1/events`. The harness reopens the feed when it drops and
    /// resumes from the last event it saw, so a plugin sees a gap in the ids
    /// rather than a silence it has to notice for itself.
    Server(Box<ServerEvent>),
}

/// Everything the harness knows, handed to the plugin when it starts.
///
/// A plugin keeps this (it is cheap to clone) and reads from it whenever it
/// needs its own identity, its settings, or the shutdown signal — for example to
/// stop a task of its own, or to `select!` against a long wait so that Ctrl-C
/// does not have to wait for it.
///
/// `S` is the plugin's own [`Sidecar::Settings`] type, parsed from the
/// `[settings]` table of its configuration file.
pub struct SidecarContext<S = NoSettings> {
    identity: ServiceIdentity,
    descriptor: ServiceDescriptor,
    config: Arc<SidecarConfig<S>>,
    marti: Option<Arc<MartiClient>>,
    control: Option<Arc<ControlClient>>,
    /// The exchange that buys this sidecar's control-API token from its
    /// orchestrator's identity, for a deployment that has no `[service] token`.
    pub(crate) workload: Option<Arc<AccessTokens>>,
    shutdown: Shutdown,
    span: Span,
}

impl<S> SidecarContext<S> {
    /// Builds the context from a loaded configuration file.
    ///
    /// This is what [`run()`](run()) does between reading the file and calling
    /// [`Sidecar::start`], and it is public because it is also how a plugin's
    /// own integration test gets a context to hand to [`drive`].
    ///
    /// `version` is the plugin's [`Sidecar::VERSION`]; it is reported in the
    /// descriptor so that the admin UI can show which build is connected.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::User`] error when the configuration
    /// describes an identity we could not assemble — see
    /// [`ServiceConfig::identity`] and [`ServerConfig::endpoints`].
    pub fn from_config(
        config: SidecarConfig<S>,
        version: &str,
        shutdown: Shutdown,
    ) -> Result<Self, Error> {
        let identity = config.identity()?;
        let descriptor = config.descriptor(version)?;
        let span = tracing::info_span!(
            "sidecar",
            service = %identity.name(),
            uid = %identity.uid(),
        );

        // Two HTTPS clients, not one: the Marti listener always presents the
        // deployment's own CA and the public listener may present an ACME or
        // operator-supplied certificate, so they are verified against different
        // roots and cannot share a client. Each is built only when something is
        // configured to call it — a plugin that only publishes CoT reads no
        // certificate it does not need. The connection pool a shared client
        // used to give away was never worth much: these are two listeners, on
        // two ports, and usually only one of them is configured at all.
        let marti = match &config.server.marti {
            Some(base) => Some(Arc::new(MartiClient::with_http(
                base,
                crate::http::client(
                    &identity,
                    crate::http::Trust::Internal,
                    crate::http::DEFAULT_TIMEOUT,
                )?,
            )?)),
            None => None,
        };

        let public = match &config.server.control {
            Some(_) => Some(crate::http::client(
                &identity,
                crate::http::Trust::Public,
                crate::http::DEFAULT_TIMEOUT,
            )?),
            None => None,
        };

        let control = match (&config.server.control, &public) {
            (Some(base), Some(http)) => Some(Arc::new(ControlClient::with_http(
                base,
                http.clone(),
                &identity,
            )?)),
            _ => None,
        };

        // `[service] token` first: an installation that configured one meant
        // it, and a deployment being migrated to workload identity should not
        // have the two racing. Otherwise the orchestrator's own identity is
        // what reaches the control API, and the deployment holds no rustak
        // secret at all.
        // The token exchange is a control-API call, so it goes over the public
        // client: `/oauth/token` is served by the same listener as `/api/v1`.
        let workload = match (
            &config.service.token,
            config.service.workload_source()?,
            &config.server.control,
            &public,
        ) {
            (None, Some(source), Some(base), Some(http)) => Some(Arc::new(AccessTokens::new(
                source,
                base.clone(),
                http.clone(),
            ))),
            _ => None,
        };

        Ok(Self {
            identity,
            descriptor,
            config: Arc::new(config),
            marti,
            control,
            workload,
            shutdown,
            span,
        })
    }

    /// The exchange that buys this sidecar's control-API token, when it has
    /// one.
    ///
    /// [`None`] for a sidecar with a `[service] token`, or with no workload
    /// identity, or with no `[server] control` to spend it at.
    pub fn workload_tokens(&self) -> Option<&Arc<AccessTokens>> {
        self.workload.as_ref()
    }

    /// The Marti API client, when `[server] marti` names one.
    ///
    /// [`None`] is a plugin that was not configured to call it, which is the
    /// ordinary case for one that only publishes CoT.
    pub fn marti(&self) -> Option<&MartiClient> {
        self.marti.as_deref()
    }

    /// The control-API client, when `[server] control` names one.
    ///
    /// The harness already registers this sidecar and reports a heartbeat on
    /// every tick through it, so this is for the rest of what the control API
    /// offers — the configuration an administrator set, a registration a plugin
    /// is retiring. To say more than "healthy", implement
    /// [`Sidecar::health`] rather than calling
    /// [`heartbeat`](ControlClient::heartbeat) here: both work, but the hook is
    /// what the harness asks for and cannot be overwritten by it.
    pub fn control(&self) -> Option<&ControlClient> {
        self.control.as_deref()
    }

    /// Who this sidecar is, including the credentials it connects with.
    pub fn identity(&self) -> &ServiceIdentity {
        &self.identity
    }

    /// What this sidecar publishes about itself — its name, version and
    /// capabilities, and nothing secret.
    pub fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }

    /// The whole configuration file, including the sections the harness reads.
    pub fn config(&self) -> &SidecarConfig<S> {
        &self.config
    }

    /// The plugin's own `[settings]` table.
    pub fn settings(&self) -> &S {
        &self.config.settings
    }

    /// The process-wide stop signal. `select!` against
    /// [`Shutdown::cancelled`] in any wait of your own that could outlast a
    /// Ctrl-C.
    pub fn shutdown(&self) -> &Shutdown {
        &self.shutdown
    }

    /// The span the harness runs the plugin inside, naming the service and its
    /// uid. Clone it into `tokio::spawn`ed work (`future.instrument(span)`) so
    /// that a plugin's own tasks are attributed to the same service.
    pub fn span(&self) -> &Span {
        &self.span
    }
}

impl<S> Clone for SidecarContext<S> {
    /// Written out rather than derived: the configuration is behind an [`Arc`],
    /// so a context clones whether or not the plugin's settings type does.
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            descriptor: self.descriptor.clone(),
            config: self.config.clone(),
            marti: self.marti.clone(),
            control: self.control.clone(),
            workload: self.workload.clone(),
            shutdown: self.shutdown.clone(),
            span: self.span.clone(),
        }
    }
}

impl<S: std::fmt::Debug> std::fmt::Debug for SidecarContext<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SidecarContext")
            .field("identity", &self.identity)
            .field("descriptor", &self.descriptor)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// A rustak plugin.
///
/// Implement this on a type that holds whatever your plugin needs to remember
/// between calls, and hand it to [`run()`](run()). Every method has a default except
/// [`start`](Sidecar::start), so the smallest useful plugin is the one in the
/// [module documentation](self).
///
/// # Why the context is only given to `start`
///
/// [`start`](Sidecar::start) receives the [`SidecarContext`] and the other
/// methods do not, because a plugin that needs it can keep it — and one that
/// does not is spared threading it through every call. Keeping it is a
/// `self.context = Some(ctx)` in `start`.
///
/// # Publishing is a return value, not a socket
///
/// [`tick`](Sidecar::tick) and [`on_event`](Sidecar::on_event) return the
/// [`Event`]s they want written to the CoT stream, and the harness writes them
/// in order. Nothing is published when the connection is down: the harness logs
/// what it dropped rather than queueing a position report that would arrive
/// minutes stale, and a plugin that must not lose an event holds it itself and
/// returns it again from the next [`Connected`](SidecarEvent::Connected).
///
/// # Errors stop the sidecar
///
/// Returning an error from any of these ends the process: the harness reports it
/// through [`rustak_core::errors::report_and_exit`] and exits with status 1. A
/// failure the plugin expects to recover from — an upstream feed that is down,
/// a heartbeat that did not go through — is therefore one to log and swallow in
/// the plugin, not one to return.
#[async_trait]
pub trait Sidecar: Send + 'static {
    /// The plugin's name, as `env!("CARGO_PKG_NAME")`.
    ///
    /// It names the telemetry session, the `--help` output and the descriptor's
    /// fallback display name. It is *not* the service name: that is what the
    /// operator puts in `[service] name`, because one plugin binary may be
    /// deployed several times against the same server.
    const NAME: &'static str;

    /// The plugin's version, as `env!("CARGO_PKG_VERSION")`. Reported to the
    /// server so an administrator can see which build is connected.
    const VERSION: &'static str;

    /// The plugin's own settings, parsed from the `[settings]` table of its
    /// configuration file.
    ///
    /// Use [`NoSettings`] if there are none. Put `#[serde(deny_unknown_fields)]`
    /// on the type: that is what makes a misspelled key a start-up failure
    /// naming the key rather than a setting that silently does nothing.
    ///
    /// The harness never logs these — a plugin's settings are its own business
    /// and may hold an upstream credential. If yours does, hold it in a
    /// [`Secret`], which redacts itself in
    /// `Debug` output and zeroises on drop, so that logging your own settings
    /// stays safe too.
    type Settings: DeserializeOwned + Default + std::fmt::Debug + Send + Sync + 'static;

    /// Called once, after the configuration has loaded and before the first
    /// [`tick`](Sidecar::tick).
    ///
    /// # Errors
    ///
    /// Anything returned here stops the sidecar before it does any work, which
    /// is where a plugin reports a setting it cannot work with.
    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error>;

    /// Called on the `[sidecar] tick` interval, starting immediately after
    /// [`start`](Sidecar::start) returns.
    ///
    /// This is where periodic work goes: polling a feed, publishing a position,
    /// and (once M6 lands) reporting a heartbeat to the control API. Whatever
    /// is returned is written to the CoT stream, in order.
    ///
    /// # Errors
    ///
    /// Returning an error stops the sidecar; see the
    /// [trait documentation](Sidecar).
    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        Ok(Vec::new())
    }

    /// What this sidecar's next heartbeat should say, asked after every
    /// [`tick`](Sidecar::tick).
    ///
    /// [`None`] — the default — is a plugin with nothing in particular to
    /// report, and the harness sends [`Heartbeat::healthy`] for it. Anything
    /// else *is* the heartbeat: the server stores the last one it was given, so
    /// the harness sends what this answers instead of its own rather than as
    /// well as it.
    ///
    /// This is the way to say more than "healthy" — a `degraded` state, a
    /// sentence for the Services page, the counters behind it. Calling
    /// [`ControlClient::heartbeat`](crate::control::ControlClient::heartbeat)
    /// through [`SidecarContext::control`] still works and is left as the escape
    /// hatch for a plugin that must report between ticks; the harness stays
    /// quiet for the tick a plugin reported in, so the two never race. Prefer
    /// this hook: it runs after the tick's work, it cannot be overwritten, and
    /// it is a value rather than a request a plugin has to remember to make.
    ///
    /// Answering is cheap and must stay that way: this is called on every tick,
    /// on the harness's own task, and a plugin that waits on an upstream here
    /// delays its own publishing. Read what the tick already worked out.
    ///
    /// A failure has nowhere to go and should not have one — a plugin that
    /// cannot say how it is doing answers [`None`] and lets the harness report
    /// the floor.
    async fn health(&mut self) -> Option<Heartbeat> {
        None
    }

    /// Called for every [`SidecarEvent`] the harness observes, with whatever
    /// the plugin wants published in reply.
    ///
    /// Answering [`Connected`](SidecarEvent::Connected) with a
    /// situational-awareness event is what gives a reconnected sidecar its
    /// callsign back; see `docs/plugins.md`.
    ///
    /// # Errors
    ///
    /// Returning an error stops the sidecar; see the
    /// [trait documentation](Sidecar).
    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        tracing::debug!(?event, "Ignoring an event this sidecar does not handle.");
        Ok(Vec::new())
    }

    /// Called once, after the shutdown signal and before the process exits.
    ///
    /// It is given `[sidecar] shutdown_grace` to finish; overrunning that is
    /// reported as a bug rather than waited on indefinitely.
    ///
    /// # Errors
    ///
    /// Returning an error makes the process exit with status 1 rather than 0.
    async fn stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_core::service::Capability;

    const CONFIG: &str = r#"
        [service]
        name = "example"
        capabilities = ["cot.publish"]

        [server]
        stream = "ssl://tak.example.com:8089"
    "#;

    fn context() -> SidecarContext<NoSettings> {
        let config: SidecarConfig<NoSettings> = rustak_core::config::load_str(CONFIG).unwrap();

        SidecarContext::from_config(config, "1.2.3", Shutdown::new()).unwrap()
    }

    #[test]
    fn the_context_carries_the_identity_the_descriptor_is_derived_from() {
        // The two halves of a service's identity (`rustak_core::service`): what
        // it *is*, and what it *publishes*. A plugin reads both from here.
        let context = context();

        assert_eq!(context.identity().name().as_str(), "example");
        assert_eq!(context.identity().uid().as_str(), "SERVICE-example");
        assert_eq!(context.descriptor().name.as_str(), "example");
        assert_eq!(context.descriptor().version.as_deref(), Some("1.2.3"));
        assert_eq!(
            context.descriptor().capabilities,
            vec![Capability::parse("cot.publish").unwrap()],
        );
        assert_eq!(
            context.descriptor().endpoints.stream.as_deref(),
            Some("ssl://tak.example.com:8089"),
        );
    }

    #[test]
    fn a_clone_shares_the_shutdown_signal_rather_than_copying_it() {
        // A plugin that clones the context into a task of its own must see the
        // same Ctrl-C the harness does, or that task outlives the drain.
        let context = context();
        let held = context.clone();

        assert!(!held.shutdown().is_cancelled());
        context.shutdown().cancel();
        assert!(held.shutdown().is_cancelled());
    }

    #[test]
    fn a_context_can_be_logged_without_leaking_the_service_token() {
        // Plugins log their context at start-up; that has to stay safe, so the
        // token is a `Secret` all the way from the file.
        let config: SidecarConfig<NoSettings> = rustak_core::config::load_str(
            r#"
            [service]
            name = "example"
            token = "rsk_supersecret"
            "#,
        )
        .unwrap();
        let context = SidecarContext::from_config(config, "1.2.3", Shutdown::new()).unwrap();

        let printed = format!("{context:?}");
        assert!(!printed.contains("rsk_supersecret"), "{printed}");
        assert!(printed.contains("example"), "{printed}");
    }

    #[test]
    fn an_event_can_be_matched_with_a_wildcard_arm() {
        // The property `#[non_exhaustive]` is here for: M1 adds variants and a
        // plugin written against M0 still compiles.
        let event = SidecarEvent::Connected {
            endpoint: "ssl://tak.example.com:8089".to_string(),
        };

        let described = match &event {
            SidecarEvent::Connected { endpoint } => endpoint.clone(),
            _ => "something else".to_string(),
        };

        assert_eq!(described, "ssl://tak.example.com:8089");
        assert_eq!(event.clone(), event);
    }
}
