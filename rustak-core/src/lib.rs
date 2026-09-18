//! The foundation every rustak binary starts from.
//!
//! `rustak-server`, `rustak-client`'s sidecar harness and every
//! `rustak-plugin-*` share the same first few seconds of life: read a TOML file
//! with secrets interpolated from the environment, bring telemetry up, arrange
//! to stop cleanly when asked, and agree on what a username, a group and a
//! secret are. That is what this crate is.
//!
//! | Module | What it is for |
//! |---|---|
//! | [`config`] | Loading TOML with `${{ env.X }}` interpolation, plus the serde adapters for durations and listen addresses |
//! | [`telemetry`] | Starting a `tracing-batteries` session before the config is read, and flushing it last |
//! | [`runtime`] | [`Shutdown`](runtime::Shutdown): one cancellation signal shared by every listener and job |
//! | [`identity`] | Usernames, secrets, argon2id hashing, group bit vectors, [`Principal`](identity::Principal) |
//! | [`service`] | A sidecar's own identity — its token and certificate paths |
//! | [`errors`] | The advice slices rustak repeats, and the one place a binary gives up |
//! | [`prelude`] | The imports every module starts with |
//!
//! # Start-up order
//!
//! The order matters and is the same in every binary:
//!
//! ```no_run
//! use rustak_core::{config, errors, runtime::Shutdown, telemetry};
//!
//! # #[derive(serde::Deserialize)] struct Config {}
//! # async fn example() {
//! // 1. The environment file first, so that `${{ env.X }}` can see it.
//! if let Err(err) = config::load_env_file(".env") {
//!     errors::report_and_exit(&err, None).await;
//! }
//!
//! // 2. Telemetry *before* the config file, so that a broken config file is
//! //    reported through it rather than in spite of it.
//! let session = telemetry::bootstrap(
//!     env!("CARGO_PKG_NAME"),
//!     env!("CARGO_PKG_VERSION"),
//!     telemetry::TelemetryOptions::from_env(),
//! );
//!
//! // 3. The shutdown signal, before anything long-lived is started.
//! let shutdown = Shutdown::new();
//! shutdown.listen_for_signals();
//!
//! // 4. And only now the configuration.
//! match config::load::<Config>("config.toml") {
//!     Ok(config) => { /* run the server */ }
//!     Err(err) => errors::report_and_exit(&err, Some(session)).await,
//! }
//! # }
//! ```
//!
//! # What is not here
//!
//! The validated identity newtypes ([`Username`](identity::Username) and the
//! rest) belong to `rustak-api`, which `rustak-ui` also depends on, and are
//! re-exported through [`identity`]. Anything touching SQLite, TLS or HTTP
//! belongs to `rustak-server`.

pub mod config;
pub mod errors;
pub mod identity;
pub mod prelude;
pub mod runtime;
pub mod service;
pub mod telemetry;
