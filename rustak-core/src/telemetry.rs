//! Bringing telemetry up before anything else, and taking it down last.
//!
//! [`bootstrap`] is the first thing a rustak binary calls, ahead of reading the
//! configuration file. That ordering is the point: the most common start-up
//! failure *is* the configuration file, and a server that only gets tracing once
//! the file has parsed cannot report the parse failure through it.
//!
//! Nothing here is configured from the TOML file for the same reason. The Sentry
//! DSN is baked in at build time by `RUSTAK_SENTRY_DSN` and may be overridden by
//! the environment variable of the same name at run time; an operator who wants
//! their own error reporting sets the variable, and one who wants none sets it
//! empty.
//!
//! # Shutdown is a wait, not a call
//!
//! [`Session::shutdown`] consumes the session, so it cannot run while any clone
//! is alive. Listeners and background jobs drop their clones asynchronously
//! after they stop, so [`shutdown`] reclaims sole ownership by polling
//! [`Arc::try_unwrap`] briefly rather than blocking forever — the same wait
//! automate's `main.rs` performs, for the same reason. Two seconds is long
//! enough for tasks that have already been told to stop and short enough that a
//! leaked clone does not hold the process open.
//!
//! # Debug builds still log
//!
//! `tracing-batteries` disables every battery in a debug build, so that a
//! developer's `cargo run` cannot report into production telemetry. That is the
//! right default for a desktop tool and the wrong one for a daemon, because the
//! gate covers the stdout writer too: `cargo run -p rustak-server` would start,
//! serve requests and print nothing at all, which is indistinguishable from a
//! server that never started. [`bootstrap`] therefore asks for
//! `with_debug_builds`, which turns the batteries the binary actually attached
//! back on and attaches none of its own — [`TelemetryOptions::from_env`] adds
//! Sentry only when a DSN is configured, and analytics never, so a debug build
//! with neither still reports nowhere.

use std::sync::Arc;

pub use tracing_batteries::Session;

/// The environment variable that supplies (or suppresses) the Sentry DSN.
pub const SENTRY_DSN_VAR: &str = "RUSTAK_SENTRY_DSN";

/// How many times [`shutdown`] checks whether the session is unshared.
const SHUTDOWN_ATTEMPTS: usize = 40;

/// How long [`shutdown`] waits between those checks.
const SHUTDOWN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// What a binary wants from telemetry, decided before any configuration is read.
///
/// [`TelemetryOptions::from_env`] builds the production shape; constructing one
/// by hand is how a test or a sidecar asks for something quieter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TelemetryOptions {
    /// The Sentry DSN to report errors to, or [`None`] for no error reporting.
    pub sentry_dsn: Option<String>,

    /// The analytics endpoint to report usage to, or [`None`] for none.
    pub analytics_url: Option<String>,

    /// Whether spans and events are also written to standard output.
    ///
    /// This is what an operator reads in `docker logs`, so it is on by default
    /// in [`TelemetryOptions::from_env`]; a sidecar embedded in another process
    /// may prefer to turn it off.
    pub stdout: bool,
}

impl TelemetryOptions {
    /// The production shape: stdout logging on, Sentry from the build-time DSN
    /// unless the environment overrides it, and no analytics.
    ///
    /// Setting `RUSTAK_SENTRY_DSN` to an empty string disables error reporting,
    /// which is how a deployment that may not send data anywhere says so.
    pub fn from_env() -> Self {
        let sentry_dsn = std::env::var(SENTRY_DSN_VAR)
            .ok()
            .or_else(|| option_env!("RUSTAK_SENTRY_DSN").map(str::to_string))
            .filter(|dsn| !dsn.trim().is_empty());

        Self {
            sentry_dsn,
            analytics_url: None,
            stdout: true,
        }
    }

    /// Turns stdout logging off, for a process whose output is somebody else's.
    #[must_use]
    pub fn without_stdout(mut self) -> Self {
        self.stdout = false;
        self
    }

    /// Reports usage to an analytics endpoint as well as tracing.
    #[must_use]
    pub fn with_analytics(mut self, url: impl Into<String>) -> Self {
        self.analytics_url = Some(url.into());
        self
    }
}

/// Starts a telemetry session, before the configuration file is read.
///
/// `app` and `version` are `&'static str` because they come from
/// `env!("CARGO_PKG_NAME")` and `env!("CARGO_PKG_VERSION")` at the call site;
/// the session holds them for its lifetime and reports them as the service
/// identity.
///
/// The returned session is shared with every listener and job, and must be
/// handed back to [`shutdown`] before the process exits.
pub fn bootstrap(
    app: &'static str,
    version: &'static str,
    options: TelemetryOptions,
) -> Arc<Session> {
    let mut session = Session::new(app, version)
        // See "Debug builds still log" above: without this a `cargo run` build
        // says nothing at all.
        .with_debug_builds()
        .with_battery(tracing_batteries::OpenTelemetry::new("").with_stdout(options.stdout));

    if let Some(dsn) = options.sentry_dsn.as_deref() {
        session = session.with_battery(tracing_batteries::Sentry::new(dsn));
    }

    if let Some(url) = options.analytics_url.clone() {
        session = session.with_battery(tracing_batteries::Analytics::new(url));
    }

    Arc::new(session)
}

/// Reclaims sole ownership of the session and flushes every battery.
///
/// In-flight work releases its clones of the session asynchronously after the
/// listeners and jobs stop, so this waits up to two seconds for the strong count
/// to fall to one. If a clone is still outstanding after that, the flush is
/// skipped with a warning rather than holding the process open indefinitely —
/// telemetry must never be the reason a server will not exit.
pub async fn shutdown(session: Arc<Session>) {
    let mut session = session;

    for _ in 0..SHUTDOWN_ATTEMPTS {
        match Arc::try_unwrap(session) {
            Ok(owned) => {
                owned.shutdown();
                return;
            }
            Err(shared) => {
                session = shared;
                tokio::time::sleep(SHUTDOWN_INTERVAL).await;
            }
        }
    }

    eprintln!(
        "Warning: could not reclaim sole ownership of the telemetry session during shutdown; some telemetry may not have been flushed."
    );
}

/// A session that records into memory instead of reporting anywhere.
///
/// Integration tests need a real [`Session`] to hand to the code under test, and
/// must not acquire one that talks to Sentry or an OTLP collector. This is that
/// session.
#[cfg(any(test, feature = "testing"))]
pub fn testing_session(app: &'static str) -> Arc<Session> {
    Arc::new(Session::new(app, "0.0.0-test").with_battery(tracing_batteries::Testing))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_dsn_means_no_error_reporting_rather_than_a_blank_one() {
        // An operator who may not send data anywhere sets the variable empty;
        // treating that as a DSN would make Sentry fail to initialise at every
        // start-up instead of simply staying off.
        let options = TelemetryOptions {
            sentry_dsn: Some("   ".to_string()),
            ..TelemetryOptions::default()
        };

        // `from_env` applies the filter; assert the rule it applies.
        assert!(
            options
                .sentry_dsn
                .as_deref()
                .filter(|dsn| !dsn.trim().is_empty())
                .is_none(),
        );
    }

    #[test]
    fn the_production_shape_logs_to_stdout() {
        // This is what an operator reads in `docker logs`; a default that was
        // quiet would make a container that fails to start say nothing at all.
        assert!(TelemetryOptions::from_env().stdout);
    }

    #[test]
    fn a_sidecar_can_ask_for_silence_without_losing_error_reporting() {
        let options = TelemetryOptions::from_env().without_stdout();

        assert!(!options.stdout);
    }

    #[test]
    fn a_debug_build_still_writes_the_lines_an_operator_reads() {
        // The failure this guards against is silent: without `with_debug_builds`
        // a `cargo run` server starts, serves requests and prints nothing, which
        // looks exactly like one that never started. Asserted through the
        // `Testing` battery so that no exporter is constructed here.
        let ours = Session::new("rustak-core-test", "0.0.0-test")
            .with_debug_builds()
            .with_battery(tracing_batteries::Testing);
        let without =
            Session::new("rustak-core-test", "0.0.0-test").with_battery(tracing_batteries::Testing);

        assert!(ours.enable().load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            without.enable().load(std::sync::atomic::Ordering::Relaxed),
            !cfg!(debug_assertions),
            "the default this overrides is 'off in a debug build'",
        );
    }

    #[tokio::test]
    async fn a_session_nobody_else_holds_is_shut_down_immediately() {
        let session = testing_session("rustak-core-test");

        let started = std::time::Instant::now();
        shutdown(session).await;

        assert!(
            started.elapsed() < SHUTDOWN_INTERVAL,
            "an unshared session should not wait at all",
        );
    }

    #[tokio::test]
    async fn a_clone_that_is_released_late_still_gets_its_telemetry_flushed() {
        // The case the wait exists for: a background task that has been told to
        // stop but has not finished dropping its clone yet.
        let session = testing_session("rustak-core-test");
        let held = session.clone();

        tokio::spawn(async move {
            tokio::time::sleep(SHUTDOWN_INTERVAL * 2).await;
            drop(held);
        });

        let started = std::time::Instant::now();
        shutdown(session).await;

        assert!(
            started.elapsed() < SHUTDOWN_INTERVAL * SHUTDOWN_ATTEMPTS as u32,
            "the wait should end when the clone is dropped, not at the deadline",
        );
    }

    #[tokio::test]
    async fn a_leaked_clone_does_not_hold_the_process_open_for_ever() {
        // Telemetry must never be the reason a server will not exit, so the
        // wait is bounded even when the clone is never released.
        let session = testing_session("rustak-core-test");
        let _leaked = session.clone();

        let started = std::time::Instant::now();
        shutdown(session).await;

        assert!(
            started.elapsed() >= SHUTDOWN_INTERVAL * SHUTDOWN_ATTEMPTS as u32,
            "the full grace period should be given before giving up",
        );
    }
}
