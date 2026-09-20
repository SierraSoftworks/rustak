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
//! # The loop
//!
//! One `select!` over four things: the shutdown signal, the tick interval, the
//! CoT stream, and the server-event feed. A tick or an inbound event becomes a
//! call into the plugin, and whatever the plugin returns is written to the
//! stream before the loop comes round again. A sidecar with no `[server] stream`
//! has a link that is never ready, and one with no `[server] control` has a feed
//! that is never ready, so it is the same loop with branches that never fire.
//!
//! # Registration and heartbeats
//!
//! A sidecar with `[server] control` registers itself before
//! [`Sidecar::start`] and reports a heartbeat after every tick, without the
//! plugin writing a line. Both are best-effort: a control API that is down is
//! logged and retried, never a reason to stop publishing CoT.
//!
//! What that heartbeat says is the plugin's to decide: the harness asks
//! [`Sidecar::health`] after each tick and sends what it answers, falling back
//! to `Heartbeat::healthy()` only for the default hook. A plugin that calls
//! [`ControlClient::heartbeat`](crate::control::ControlClient::heartbeat)
//! itself instead keeps the tick it reported in — the harness stays quiet
//! rather than overwriting it a moment later.
//!
//! The connection is opened *before* [`Sidecar::start`], so an operator who got
//! the connect string or the certificate paths wrong is told that rather than
//! whatever the plugin's own start-up does about it.
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
use futures::StreamExt;
use rustak_core::config;
use rustak_core::errors::report_and_exit;
use rustak_core::prelude::*;
use rustak_core::runtime::with_grace;
use rustak_core::telemetry::{self, TelemetryOptions};
use rustak_cot::Event;
use tracing::Instrument;

use super::{ControlLink, Link, Sidecar, SidecarConfig, SidecarContext, SidecarEvent};

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

/// Drives a sidecar: connect, start, tick and dispatch until shutdown, stop.
///
/// This is the loop [`run`] ends in, and it is public because it is also how a
/// plugin's own integration test exercises its implementation — build a
/// [`SidecarContext`] with [`SidecarContext::from_config`], call this, and
/// cancel the context's [`Shutdown`] to end it. A context whose `[server]
/// stream` is unset drives the plugin without opening a socket, which is what
/// makes that test offline.
///
/// # Errors
///
/// Returns whatever the sidecar returned. A `[server] stream` that cannot be
/// read, or a TLS endpoint without certificate material, fails before
/// [`start`](Sidecar::start) is called; any error from the plugin's own methods
/// ends the loop; and [`stop`](Sidecar::stop) overrunning its grace period is
/// reported as a [`human_errors::Kind::System`] error. A connection that drops
/// is *not* an error: it is reported to the plugin as
/// [`SidecarEvent::Disconnected`] and reopened with backoff.
pub async fn drive<S: Sidecar>(
    sidecar: &mut S,
    context: SidecarContext<S::Settings>,
) -> Result<(), Error> {
    let span = context.span().clone();

    tick_until_shutdown(sidecar, context).instrument(span).await
}

/// What woke the harness loop, decided while the [`Link`] is still borrowed and
/// acted on once it is not.
///
/// `tokio::select!` keeps every branch's future alive while it evaluates the
/// handler, so a handler that both read from the link and wrote to it would not
/// borrow-check. Naming the two cases instead keeps the loop one loop.
enum Woken {
    /// The tick interval came round.
    Tick,
    /// The CoT stream produced something.
    Stream(SidecarEvent),
    /// The server-event feed produced something.
    Server(SidecarEvent),
}

/// [`drive`], inside the context's span.
async fn tick_until_shutdown<S: Sidecar>(
    sidecar: &mut S,
    context: SidecarContext<S::Settings>,
) -> Result<(), Error> {
    let shutdown = context.shutdown().clone();
    let interval = context.config().sidecar.tick();
    let grace = context.config().sidecar.shutdown_grace();

    // Before `start`, so that a connect string or a certificate the operator
    // got wrong is reported instead of the plugin's own start-up work.
    let mut link = Link::open(&context)?;
    let mut control = ControlLink::open(&context);

    // Registration is best-effort and happens before `start`, so that a plugin
    // whose own start-up reads its per-service configuration finds a
    // registration to read it from. A server that refused it is logged, not
    // fatal — see `ControlLink`.
    control.register().await;

    sidecar.start(context).await?;
    tracing::info!(?interval, stream = ?link.endpoint(), "The sidecar has started.");

    let mut ticker = tokio::time::interval(interval);
    // A tick we were too busy to take is a tick to take late rather than one to
    // take twice in a row: a plugin whose work overruns its interval should fall
    // behind, not stampede.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let woken = tokio::select! {
            // Biased so that a sidecar being stopped never takes one more tick
            // because two branches happened to be ready at once.
            biased;

            () = shutdown.cancelled() => break,
            _ = ticker.tick() => Woken::Tick,
            Some(event) = link.next() => Woken::Stream(event),
            Some(event) = control.next() => Woken::Server(SidecarEvent::Server(Box::new(event))),
        };

        let published: Vec<Event> = match woken {
            Woken::Tick => {
                let published = sidecar.tick().await?;

                // After the plugin's own tick, so that a heartbeat says how the
                // sidecar is *after* the work rather than before it, and from
                // the plugin's own hook so that the harness reports the floor
                // rather than flattening what the plugin had to say.
                control.report(sidecar.health().await).await;

                published
            }
            Woken::Stream(event) | Woken::Server(event) => sidecar.on_event(event).await?,
        };

        // Raced against the shutdown because a write into a connection that is
        // reconnecting waits for it: Ctrl-C must not have to wait as well.
        tokio::select! {
            biased;

            () = shutdown.cancelled() => break,
            result = link.publish(published) => result?,
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
        /// What every tick publishes, for the tests that watch the wire.
        publishes: Vec<Event>,
        /// Where every event this sidecar is handed is reported, for the tests
        /// that assert on the sequence rather than the count.
        seen: Option<tokio::sync::mpsc::UnboundedSender<SidecarEvent>>,
        /// What the health hook answers, for the test that watches the control
        /// API rather than the wire.
        health: Option<rustak_api::Heartbeat>,
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

        async fn tick(&mut self) -> Result<Vec<Event>, Error> {
            self.ticks += 1;

            if self.fail_on_tick {
                return Err(human_errors::user("The upstream feed is unreachable.", &[]));
            }

            if self.ticks >= self.stop_after
                && let Some(context) = &self.context
            {
                context.shutdown().cancel();
            }

            Ok(self.publishes.clone())
        }

        async fn health(&mut self) -> Option<rustak_api::Heartbeat> {
            self.health.clone()
        }

        async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
            self.events += 1;

            if let Some(seen) = &self.seen {
                let _ = seen.send(event);
            }

            Ok(Vec::new())
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
        context_for(tick_ms, grace_ms, None)
    }

    fn context_for(
        tick_ms: i64,
        grace_ms: i64,
        stream: Option<String>,
    ) -> SidecarContext<NoSettings> {
        context_with(tick_ms, grace_ms, stream, None)
    }

    fn context_with(
        tick_ms: i64,
        grace_ms: i64,
        stream: Option<String>,
        control: Option<String>,
    ) -> SidecarContext<NoSettings> {
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
            server: ServerConfig {
                stream,
                control,
                ..ServerConfig::default()
            },
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
    async fn the_loop_reports_what_the_health_hook_answers_rather_than_its_own_healthy() {
        // The hook, through the loop: three ticks, three heartbeats, and every
        // one of them the plugin's own words. A harness that still sent
        // `healthy()` afterwards would show up here as six.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "state": "degraded" })),
            )
            .mount(&server)
            .await;

        let reported = rustak_api::Heartbeat {
            state: rustak_api::ServiceState::Degraded,
            message: Some("The upstream is slow.".into()),
            metrics: serde_json::json!({ "queue_depth": 3 }),
        };
        let mut sidecar = Counter {
            stop_after: 3,
            health: Some(reported.clone()),
            ..Counter::default()
        };

        drive(
            &mut sidecar,
            context_with(1, 1_000, None, Some(server.uri())),
        )
        .await
        .unwrap();

        let beats: Vec<rustak_api::Heartbeat> = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path().ends_with("/heartbeat"))
            .map(|request| request.body_json().expect("a heartbeat body"))
            .collect();

        assert_eq!(beats.len(), 3, "one per tick, and no floor over the top");
        assert!(beats.iter().all(|beat| beat == &reported), "{beats:?}");
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

    /// The full round trip: connect, negotiate, receive, publish, drop,
    /// reconnect — against a real listener, because reconnection is the one
    /// thing an in-memory duplex cannot be redialled to prove.
    ///
    /// Time is not paused here. Tokio's auto-advance races real socket
    /// readiness, and the assertions are event-driven rather than timed: every
    /// wait is for something to arrive, bounded by a timeout that only fires
    /// when the test has genuinely failed. The one real wait is the reconnect's
    /// [`MIN_BACKOFF`](crate::stream::MIN_BACKOFF) second.
    #[tokio::test]
    async fn the_harness_connects_receives_publishes_and_reconnects() {
        use crate::stream::testing::Eud;
        use rustak_cot::{CotTime, negotiate};
        use std::time::Duration;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut peer = Eud::over(socket, "ANDROID-1", "BRAVO");

            // Offer TAK Protocol v1 and then refuse the request that comes
            // back: the exchange that leaves a connection on XML, which is the
            // one a CloudTAK-style server performs.
            let now = CotTime::now();
            let offer = negotiate::announce("NEG-1", "rustak-test", negotiate::API_VERSION, now);
            peer.send(offer).await.unwrap();
            peer.send(negotiate::response("NEG-1", false, now))
                .await
                .unwrap();
            peer.send_sa(51.5074, -0.1278).await.unwrap();

            let published = peer
                .expect_uid("SERVICE-example", Duration::from_secs(5))
                .await
                .expect("the sidecar's tick should reach the wire");

            // Dropping the socket is what the sidecar has to survive.
            drop(peer);

            tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .expect("the sidecar should dial again")
                .unwrap();

            published
        });

        let (seen, mut arrived) = tokio::sync::mpsc::unbounded_channel();
        let context = context_for(50, 1_000, Some(format!("tcp://127.0.0.1:{port}")));
        let shutdown = context.shutdown().clone();
        let mut sidecar = Counter {
            stop_after: usize::MAX,
            seen: Some(seen),
            publishes: vec![
                Event::builder("a-f-G-U-C", "SERVICE-example")
                    .point(48.85, 2.35)
                    .build(),
            ],
            ..Counter::default()
        };

        let driving = tokio::spawn(async move {
            let result = drive(&mut sidecar, context).await;

            (result, sidecar)
        });

        let mut sequence: Vec<SidecarEvent> = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(event) = arrived.recv().await {
                let reconnected = matches!(event, SidecarEvent::Connected { .. })
                    && sequence
                        .iter()
                        .any(|seen| matches!(seen, SidecarEvent::Disconnected { .. }));

                sequence.push(event);

                if reconnected {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the sidecar never completed the cycle: {sequence:?}"));

        shutdown.cancel();
        let (result, sidecar) = driving.await.unwrap();
        result.expect("a dropped connection is not a reason to stop the sidecar");
        assert!(sidecar.stopped);

        let published = server.await.unwrap();
        assert_eq!(published.uid, "SERVICE-example");
        assert_eq!(published.r#type, "a-f-G-U-C");

        assert!(
            matches!(sequence.first(), Some(SidecarEvent::Connected { endpoint }) if endpoint.contains(&port.to_string())),
            "{sequence:?}",
        );
        assert!(
            sequence
                .iter()
                .any(|event| matches!(event, SidecarEvent::Negotiated { protobuf: false })),
            "{sequence:?}",
        );
        assert!(
            sequence.iter().any(
                |event| matches!(event, SidecarEvent::Cot(cot) if cot.uid == "ANDROID-1" && cot.callsign() == Some("BRAVO")),
            ),
            "{sequence:?}",
        );
        assert!(
            matches!(sequence.last(), Some(SidecarEvent::Connected { .. })),
            "{sequence:?}",
        );
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
