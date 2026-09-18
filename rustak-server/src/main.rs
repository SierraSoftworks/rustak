//! `rustak` — the server binary.
//!
//! This file is deliberately thin: it parses arguments, loads the environment
//! file and the configuration, and hands off. The start-up sequence proper —
//! telemetry, the shutdown signal, the listeners and the job host — is wired up
//! in `rustak_server::run` by the bootstrap brief, and this file will call it
//! rather than growing one of its own.
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
#[allow(clippy::print_stdout)]
async fn main() {
    // `--check` is the one thing here that reports success, and it reports it
    // on stdout so that a pipeline can capture it; everything else this binary
    // says goes through tracing once telemetry is up.
    let args = Args::parse();

    // The environment file first: a `${{ env.X }}` expression in the
    // configuration is substituted from whatever this puts in place.
    if let Err(err) = rustak_core::config::load_env_file(&args.env) {
        report_and_exit(&err, None).await;
    }

    let config = match Config::load(&args.config) {
        Ok(config) => config,
        // No telemetry session to flush: a configuration we could not read is
        // by definition the operator's to fix, not a fault to report.
        Err(err) => report_and_exit(&err, None).await,
    };

    if args.check {
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
        return;
    }

    // The run loop lands in the bootstrap brief (design 01 §3.4): telemetry
    // bootstrap, `Shutdown::listen_for_signals`, `rustak_server::run`, and the
    // telemetry flush that has to outlive it.
    eprintln!(
        "rustak {}: the configuration loaded, but this build has no run loop yet. \
         Use --check to validate a configuration file.",
        env!("CARGO_PKG_VERSION"),
    );
}
