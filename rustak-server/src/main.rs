//! `rustak` — the server binary.
//!
//! This file is deliberately thin: it parses arguments, loads the environment
//! file and the configuration, brings telemetry and the shutdown signal up, and
//! hands off to [`rustak_server::run`]. Everything the server *is* lives in the
//! library, so an integration test can start the same server in-process.
//!
//! # The order start-up happens in
//!
//! The environment file first, because `${{ env.X }}` in the configuration is
//! substituted from whatever it puts in place. Then telemetry, *before* the
//! configuration is read, because the most common start-up failure is the
//! configuration file and a server that only gets tracing once the file has
//! parsed cannot report the parse failure through it. Then the signal handler,
//! then the run loop.
//!
//! # `--check`
//!
//! `rustak --config config.toml --check` loads and validates the file and
//! exits: 0 when it would start, 1 with the failure printed when it would not.
//! It deliberately does **not** bring telemetry up or touch the data directory,
//! so it is safe to run in a deployment pipeline against a candidate file, as
//! many times as you like, on a machine that is not the server.

use std::path::PathBuf;

use clap::Parser;
use rustak_core::errors::report_and_exit;
use rustak_core::runtime::Shutdown;
use rustak_core::telemetry::{self, TelemetryOptions};
use rustak_server::config::Config;

/// The command line.
#[derive(Debug, Parser)]
#[command(name = "rustak", version, about = "A single-binary TAK server.")]
struct Args {
    /// The configuration file to load.
    #[arg(
        long,
        short,
        value_name = "FILE",
        default_value = "config.toml",
        env = "RUSTAK_CONFIG"
    )]
    config: PathBuf,

    /// An environment file, loaded over the process environment before the
    /// configuration is read, so that `${{ env.X }}` can see it.
    #[arg(
        long,
        value_name = "FILE",
        default_value = ".env",
        env = "RUSTAK_ENV_FILE"
    )]
    env: PathBuf,

    /// Load and validate the configuration, then exit without starting.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // The environment file first: a `${{ env.X }}` expression in the
    // configuration is substituted from whatever this puts in place.
    if let Err(err) = rustak_core::config::load_env_file(&args.env) {
        report_and_exit(&err, None).await;
    }

    if args.check {
        check(&args).await;
        return;
    }

    // Telemetry before the configuration, for the reason in the module
    // documentation. From here on every failure is reported through the
    // session, which is flushed before the process exits.
    let session = telemetry::bootstrap(
        "rustak",
        env!("CARGO_PKG_VERSION"),
        TelemetryOptions::from_env(),
    );

    let config = match Config::load(&args.config) {
        Ok(config) => config,
        Err(err) => report_and_exit(&err, Some(session)).await,
    };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    if let Err(err) = rustak_server::run(config, session.clone(), shutdown.clone()).await {
        report_and_exit(&err, Some(session)).await;
    }

    // Last, and after everything holding a clone of it has stopped: the flush
    // needs sole ownership of the session, which is why `run` takes a clone
    // rather than the session itself.
    telemetry::shutdown(session).await;

    // A second signal is an operator saying they are not waiting for the
    // drain, and the process should say so: `run` returns `Ok` either way —
    // it checkpointed the database on both paths — so a status of 0 would
    // make "stopped cleanly" and "cut off mid-drain" indistinguishable to
    // whatever is supervising. 130 is the conventional "ended by a signal",
    // and it is only ever reached after the flush above.
    if shutdown.is_aborted() {
        std::process::exit(130);
    }
}

/// `--check`: validate the file and say what it would do.
///
/// Reports on stdout rather than through tracing, because a pipeline capturing
/// the answer should not have to parse log lines to find it — and because
/// telemetry is deliberately not up.
#[allow(clippy::print_stdout)]
async fn check(args: &Args) {
    let config = match Config::load(&args.config) {
        Ok(config) => config,
        // No telemetry session to flush: a configuration we could not read is
        // by definition the operator's to fix, not a fault to report.
        Err(err) => report_and_exit(&err, None).await,
    };

    println!(
        "{} is valid: {} would listen on {}, with data in {}.",
        args.config.display(),
        config.server.name,
        config
            .web
            .public
            .listen
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        config.server.data_dir.display(),
    );
}
