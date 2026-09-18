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
//! # What M0 wires up, and what it does not
//!
//! The harness loads the configuration, brings telemetry up, builds the
//! [`ServiceIdentity`] and the [`ServiceDescriptor`] the plugin will register
//! with, calls [`Sidecar::start`], calls [`Sidecar::tick`] on an interval, and
//! calls [`Sidecar::stop`] when the process is asked to stop.
//!
//! It does **not** yet connect to anything: the CoT stream client lands in M1,
//! the Marti client in M2 and the `/api/v1/services/*` control client in M6. The
//! shapes those will arrive through are already here — [`Sidecar::on_event`] and
//! [`SidecarEvent`] — so that a plugin written today keeps compiling when they
//! do. [`SidecarEvent`] is `#[non_exhaustive]` for the same reason: match it
//! with a `_` arm and new variants are additive.
//!
//! See `docs/plugins.md` for the operator-facing version of all of this.

pub mod config;
pub mod run;

use std::sync::Arc;

use rustak_core::prelude::*;
use rustak_core::service::{ServiceDescriptor, ServiceIdentity};
use tracing::Span;

pub use async_trait::async_trait;
pub use config::{HarnessConfig, NoSettings, ServerConfig, ServiceConfig, SidecarConfig};
pub use run::{Args, drive, run, run_with};

/// Something the sidecar harness noticed that a plugin may want to react to.
///
/// Every variant is delivered to [`Sidecar::on_event`]. The enum is
/// `#[non_exhaustive]`, so a plugin matches it with a `_` arm and keeps
/// compiling as the stream, Marti and control clients land in later milestones.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SidecarEvent {
    /// The CoT stream connection came up, against the endpoint named.
    Connected {
        /// The connect string that was dialled, e.g. `ssl://tak.example.com:8089`.
        endpoint: String,
    },

    /// The CoT stream connection dropped. The harness reconnects with backoff;
    /// this is for a plugin that wants to count outages or pause its own work.
    Disconnected {
        /// Why the connection ended, for logging rather than for matching on.
        reason: String,
    },

    /// A message arrived on the CoT stream.
    ///
    /// Boxed because a TAK message is an order of magnitude larger than the
    /// other variants and a plugin passes events around by value.
    ///
    /// M1 replaces this payload with the parsed CoT event model once
    /// `rustak-cot` has one; the `_` arm a `#[non_exhaustive]` enum already
    /// obliges you to write is what makes that a compatible change.
    Cot(Box<rustak_cot::proto::TakMessage>),
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

        Ok(Self {
            identity,
            descriptor,
            config: Arc::new(config),
            shutdown,
            span,
        })
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
    /// and (once M6 lands) reporting a heartbeat to the control API.
    ///
    /// # Errors
    ///
    /// Returning an error stops the sidecar; see the
    /// [trait documentation](Sidecar).
    async fn tick(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Called for every [`SidecarEvent`] the harness observes.
    ///
    /// Nothing produces events in M0 — the stream client lands in M1 — so the
    /// default implementation is the right one until then.
    ///
    /// # Errors
    ///
    /// Returning an error stops the sidecar; see the
    /// [trait documentation](Sidecar).
    async fn on_event(&mut self, event: SidecarEvent) -> Result<(), Error> {
        tracing::debug!(?event, "Ignoring an event this sidecar does not handle.");
        Ok(())
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
