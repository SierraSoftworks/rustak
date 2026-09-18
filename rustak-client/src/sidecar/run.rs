//! The harness: a command line, a start-up sequence and a loop.
//!
//! [`run`] is the whole of a plugin's `main`. It performs the start-up sequence
//! every rustak binary shares — environment file, telemetry, shutdown signal,
//! configuration, in that order and for the reasons `rustak_core` gives — and
//! then drives the plugin's [`Sidecar`] implementation until the process is
//! asked to stop.
//!
//! # The command line
//!
//! Every plugin gets the same two options, named after the plugin itself in
//! `--help` and `--version`:
//!
//! ```text
//! rustak-plugin-example --config plugin.toml [--env .env] [--check]
//! ```
//!
//! `--check` loads and validates the configuration and exits, so a deployment
//! pipeline can test a candidate file against the binary that will read it.
//!
//! A plugin that needs options of its own parses its own [`clap`] type and calls
//! [`run_with`] with an [`Args`] it has filled in.
//!
//! # Stopping
//!
//! `SIGINT`/`SIGTERM` cancels the [`Shutdown`] the context carries; the tick
//! loop notices at its next turn, [`Sidecar::stop`] is given
//! `[sidecar] shutdown_grace` to finish, telemetry is flushed, and the process
//! exits 0. A second signal exits immediately with status 130 — see
//! [`rustak_core::runtime`].

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};
use rustak_core::config;
use rustak_core::errors::report_and_exit;
use rustak_core::prelude::*;
use rustak_core::runtime::with_grace;
use rustak_core::telemetry::{self, TelemetryOptions};
use tracing::Instrument;

use super::{Sidecar, SidecarConfig, SidecarContext};

/// The command line every sidecar shares.
#[derive(Clone, Debug, Parser)]
#[command(about = "A rustak sidecar.")]
pub struct Args {
    /// The configuration file to load.
    #[arg(
        long,
        short,
        value_name = "FILE",
        default_value = "config.toml",
        env = "RUSTAK_SIDECAR_CONFIG"
    )]
    pub config: PathBuf,

    /// An environment file, loaded over the process environment before the
    /// configuration is read, so that `${{ env.X }}` can see it.
    #[arg(
        long,
        value_name = "FILE",
        default_value = ".env",
        env = "RUSTAK_ENV_FILE"
    )]
    pub env: PathBuf,

    /// Load and validate the configuration, then exit without starting.
    #[arg(long)]
    pub check: bool,
}

impl Args {
    /// Parses the command line, naming and versioning it after the plugin.
    ///
    /// `--help` and `--version` come from the plugin's own
    /// [`NAME`](Sidecar::NAME) and [`VERSION`](Sidecar::VERSION) rather than
    /// from `rustak-client`, which is what an operator running
    /// `rustak-plugin-adsb --version` expects to see.
    pub fn for_sidecar<S: Sidecar>() -> Self {
        let command = Self::command().name(S::NAME).version(S::VERSION);

        match Self::from_arg_matches(&command.get_matches()) {
            Ok(args) => args,
            // clap prints the usage message itself and exits with the
            // conventional status; there is nothing for us to add.
            Err(err) => err.exit(),
        }
    }
}

/// Runs a sidecar: the whole of a plugin's `main`.
///
/// Parses the command line, loads the configuration, brings telemetry up, and
/// drives `S` until the process is asked to stop. Any failure is reported
/// through [`report_and_exit`], which prints it, records it if it is ours, and
/// exits with status 1.
///
/// `S: Default` because the harness constructs the sidecar before the
/// configuration exists; anything a plugin needs from the file it takes in
/// [`Sidecar::start`]. A plugin that cannot be built that way calls
/// [`run_with`] instead.
pub async fn run<S: Sidecar + Default>() {
    let args = Args::for_sidecar::<S>();

    run_with(S::default(), args).await;
}

/// Runs a sidecar that has already been constructed, with a command line that
/// has already been parsed.
///
/// This is [`run`] for a plugin with options of its own: parse your own
/// [`clap`] type, fill in an [`Args`] from it, and call this.
pub async fn run_with<S: Sidecar>(sidecar: S, args: Args) {
    // The environment file first, so that `${{ env.X }}` in the configuration
    // can see what it puts in place. Nothing is up yet to report a failure
    // through, and a `.env` we cannot read is by definition the operator's.
    if let Err(err) = config::load_env_file(&args.env) {
        report_and_exit(&err, None).await;
    }

    // Telemetry before the configuration, because the most common start-up
    // failure *is* the configuration file. `bootstrap` is also what keeps a
    // debug build's log lines flowing — see `rustak_core::telemetry` — so a
    // plugin author running `cargo run` can see their own heartbeats.
    let session = telemetry::bootstrap(S::NAME, S::VERSION, TelemetryOptions::from_env());

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    if let Err(err) = serve(sidecar, &args, shutdown).await {
        report_and_exit(&err, Some(session)).await;
    }

    telemetry::shutdown(session).await;
}

/// Everything [`run_with`] does once telemetry and the shutdown signal are up,
/// in a form that returns its failures instead of exiting.
async fn serve<S: Sidecar>(mut sidecar: S, args: &Args, shutdown: Shutdown) -> Result<(), Error> {
    let config: SidecarConfig<S::Settings> = config::load(&args.config)?;
    let context = SidecarContext::from_config(config, S::VERSION, shutdown)?;

    // The descriptor is what this sidecar publishes about itself, so it is safe
    // to log in full. The plugin's own `[settings]` deliberately are not logged:
    // they are whatever the plugin says they are, which may include an upstream
    // API key, and a harness that printed them would make that a trap rather
    // than a decision.
    tracing::info!(
        descriptor = ?context.descriptor(),
        file = %args.config.display(),
        "Loaded the configuration for {} {}.",
        S::NAME,
        S::VERSION,
    );

    if args.check {
        tracing::info!("The configuration is valid; --check does not start the sidecar.");
        return Ok(());
    }

    drive(&mut sidecar, context).await
}

/// Drives a sidecar: start, tick until shutdown, stop.
///
/// This is the loop [`run`] ends in, and it is public because it is also how a
/// plugin's own integration test exercises its implementation — build a
/// [`SidecarContext`] with [`SidecarContext::from_config`], call this, and
/// cancel the context's [`Shutdown`] to end it.
///
/// # Errors
///
/// Returns whatever the sidecar returned. Any error from
/// [`start`](Sidecar::start) or [`tick`](Sidecar::tick) ends the loop, and
/// [`stop`](Sidecar::stop) overrunning its grace period is reported as a
/// [`human_errors::Kind::System`] error.
pub async fn drive<S: Sidecar>(
    sidecar: &mut S,
    context: SidecarContext<S::Settings>,
) -> Result<(), Error> {
    let span = context.span().clone();

    tick_until_shutdown(sidecar, context).instrument(span).await
}

/// [`drive`], inside the context's span.
async fn tick_until_shutdown<S: Sidecar>(
    sidecar: &mut S,
    context: SidecarContext<S::Settings>,
) -> Result<(), Error> {
    let shutdown = context.shutdown().clone();
    let interval = context.config().sidecar.tick();
    let grace = context.config().sidecar.shutdown_grace();

    sidecar.start(context).await?;
    tracing::info!(?interval, "The sidecar has started.");

    let mut ticker = tokio::time::interval(interval);
    // A tick we were too busy to take is a tick to take late rather than one to
    // take twice in a row: a plugin whose work overruns its interval should fall
    // behind, not stampede.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // Biased so that a sidecar being stopped never takes one more tick
            // because two branches happened to be ready at once.
            biased;

            () = shutdown.cancelled() => break,
            _ = ticker.tick() => sidecar.tick().await?,
        }
    }

    tracing::info!("The sidecar is stopping.");
    with_grace("the sidecar", sidecar.stop(), grace).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{NoSettings, ServerConfig, ServiceConfig, SidecarEvent, async_trait};

    /// A sidecar that counts what it was asked to do and stops itself after
    /// `stop_after` ticks, so that these tests never depend on a wall clock.
    #[derive(Default)]
    struct Counter {
        context: Option<SidecarContext<NoSettings>>,
        ticks: usize,
        events: usize,
        stopped: bool,
        stop_after: usize,
        fail_on_tick: bool,
        hang_on_stop: bool,
    }

    #[async_trait]
    impl Sidecar for Counter {
        const NAME: &'static str = "rustak-client-test";
        const VERSION: &'static str = "0.0.0-test";
        type Settings = NoSettings;

        async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
            self.context = Some(ctx);
            Ok(())
        }

        async fn tick(&mut self) -> Result<(), Error> {
            self.ticks += 1;

            if self.fail_on_tick {
                return Err(human_errors::user("The upstream feed is unreachable.", &[]));
            }

            if self.ticks >= self.stop_after
                && let Some(context) = &self.context
            {
                context.shutdown().cancel();
            }

            Ok(())
        }

        async fn on_event(&mut self, _event: SidecarEvent) -> Result<(), Error> {
            self.events += 1;
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), Error> {
            self.stopped = true;

            if self.hang_on_stop {
                std::future::pending::<()>().await;
            }

            Ok(())
        }
    }

    fn context(tick_ms: i64, grace_ms: i64) -> SidecarContext<NoSettings> {
        let config = SidecarConfig {
            service: ServiceConfig {
                name: ServiceName::parse("example").unwrap(),
                display_name: None,
                capabilities: Vec::new(),
                token: None,
                certificate: None,
                key: None,
                truststore: None,
            },
            server: ServerConfig::default(),
            sidecar: crate::sidecar::HarnessConfig {
                tick: chrono::Duration::milliseconds(tick_ms),
                shutdown_grace: chrono::Duration::milliseconds(grace_ms),
            },
            settings: NoSettings {},
        };

        SidecarContext::from_config(config, Counter::VERSION, Shutdown::new()).unwrap()
    }

    #[tokio::test]
    async fn a_sidecar_ticks_until_it_is_told_to_stop_and_is_then_stopped() {
        // The whole loop: start, tick on an interval, notice the cancellation,
        // stop. The sidecar cancels its own shutdown so the test is about the
        // sequence rather than about timing.
        let mut sidecar = Counter {
            stop_after: 3,
            ..Counter::default()
        };

        drive(&mut sidecar, context(1, 1_000)).await.unwrap();

        assert_eq!(sidecar.ticks, 3);
        assert!(sidecar.stopped);
    }

    #[tokio::test]
    async fn the_first_tick_happens_at_once_rather_than_an_interval_later() {
        // A sidecar configured to tick hourly still does its first piece of work
        // when it starts; an operator restarting a plugin expects it to do
        // something they can see.
        let mut sidecar = Counter {
            stop_after: 1,
            ..Counter::default()
        };

        let started = std::time::Instant::now();
        drive(&mut sidecar, context(3_600_000, 1_000))
            .await
            .unwrap();

        assert_eq!(sidecar.ticks, 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_sidecar_that_is_cancelled_before_it_starts_stops_without_ticking() {
        // The Ctrl-C that arrives during start-up: the loop must not take a
        // tick it was already told not to take.
        let mut sidecar = Counter {
            stop_after: usize::MAX,
            ..Counter::default()
        };
        let context = context(1, 1_000);
        context.shutdown().cancel();

        drive(&mut sidecar, context).await.unwrap();

        assert_eq!(sidecar.ticks, 0);
        assert!(sidecar.stopped);
    }

    #[tokio::test]
    async fn an_error_from_a_tick_ends_the_sidecar() {
        // Documented behaviour: a failure a plugin expects to recover from is
        // one it swallows, so anything that reaches the harness is fatal.
        let mut sidecar = Counter {
            stop_after: usize::MAX,
            fail_on_tick: true,
            ..Counter::default()
        };

        let Err(err) = drive(&mut sidecar, context(1, 1_000)).await else {
            panic!("a failing tick should stop the sidecar");
        };

        assert!(err.to_string().contains("upstream feed"), "{err}");
    }

    #[tokio::test]
    async fn a_stop_that_never_finishes_does_not_hold_the_process_open() {
        // A shutdown that can be blocked indefinitely is a shutdown that ends in
        // `kill -9`, so the grace period is bounded and the overrun is our bug.
        let mut sidecar = Counter {
            stop_after: 1,
            hang_on_stop: true,
            ..Counter::default()
        };

        let Err(err) = drive(&mut sidecar, context(1, 50)).await else {
            panic!("a stop that never returns should not pass the grace period");
        };

        assert!(err.is(human_errors::Kind::System), "{err}");
        assert!(err.to_string().contains("the sidecar"), "{err}");
    }

    #[tokio::test]
    async fn events_reach_the_sidecar_that_was_written_for_them() {
        // Nothing produces events until M1; this is the seam that proves the
        // dispatch a plugin writes against today keeps working then.
        let mut sidecar = Counter::default();

        sidecar
            .on_event(SidecarEvent::Connected {
                endpoint: "ssl://tak.example.com:8089".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(sidecar.events, 1);
    }

    #[tokio::test]
    async fn check_validates_the_file_without_starting_the_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plugin.toml");
        std::fs::write(&path, "[service]\nname = \"example\"\n").unwrap();

        let args = Args {
            config: path,
            env: directory.path().join("absent.env"),
            check: true,
        };

        serve(Counter::default(), &args, Shutdown::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_configuration_that_does_not_load_is_reported_rather_than_started() {
        let args = Args {
            config: PathBuf::from("/rustak/definitely/not/here.toml"),
            env: PathBuf::from("/rustak/definitely/not/here.env"),
            check: true,
        };

        let Err(err) = serve(Counter::default(), &args, Shutdown::new()).await else {
            panic!("a missing configuration file should not start a sidecar");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("not/here.toml"), "{err}");
    }

    #[test]
    fn the_command_line_defaults_to_the_files_a_container_image_mounts() {
        let args = Args::parse_from(["rustak-plugin-example"]);

        assert_eq!(args.config, PathBuf::from("config.toml"));
        assert_eq!(args.env, PathBuf::from(".env"));
        assert!(!args.check);

        let given = Args::parse_from(["rustak-plugin-example", "--config", "/data/plugin.toml"]);
        assert_eq!(given.config, PathBuf::from("/data/plugin.toml"));
    }

    #[test]
    fn the_command_line_is_named_after_the_plugin_rather_than_the_sdk() {
        // `rustak-plugin-adsb --version` should say the plugin's version; the
        // SDK's would be a support conversation that goes nowhere.
        let command = Args::command()
            .name(Counter::NAME)
            .version(Counter::VERSION);

        assert_eq!(command.get_name(), "rustak-client-test");
        assert_eq!(command.get_version(), Some("0.0.0-test"));
    }
}
