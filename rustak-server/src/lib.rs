//! rustak server library: listeners, SQLite, PKI/ACME, Marti API, OAuth2,
//! admin API, embedded UI.
//!
//! `src/main.rs` is a thin CLI entry point; this crate exposes the app itself
//! so integration tests under `tests/` can build and drive it in-process. The
//! module tree follows `.claude/plan/design/01-foundations-storage-ci.md` §3.1.
//!
//! # Starting up
//!
//! [`run`] is the whole server: it opens the storage every module reaches
//! through ([`build_context`]) and then hands the context to
//! [`runtime::run_all`], which binds the listeners and consumes the job queue
//! until the [`Shutdown`] it was given is cancelled.
//!
//! The split exists because the two halves fail differently. Everything
//! [`build_context`] does is a file or a key: a data directory that cannot be
//! written, a database that will not migrate, an encryption key that no longer
//! opens what it sealed. Those failures happen once, before anything is
//! listening, and an operator fixes them and starts again. Everything
//! [`runtime::run_all`] does is a socket or a task, and those have to be
//! stopped as carefully as they were started.
//!
//! ```no_run
//! # async fn example() -> Result<(), human_errors::Error> {
//! use rustak_core::{prelude::*, telemetry::{self, TelemetryOptions}};
//! use rustak_server::config::Config;
//!
//! let session = telemetry::bootstrap("rustak", "0.0.0", TelemetryOptions::from_env());
//! let shutdown = Shutdown::new();
//! shutdown.listen_for_signals();
//!
//! rustak_server::run(Config::load("config.toml")?, session, shutdown).await
//! # }
//! ```

pub mod auth;
pub mod config;
pub mod cot_store;
pub mod crypto;
pub mod db;
pub mod files;
pub mod identity;
pub mod jobs;
pub mod marti;
pub mod missions;
pub mod pki;
pub mod plugins;
pub mod prelude;
pub mod profiles;
pub mod runtime;
pub mod services;
pub mod store;
pub mod stream;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod web;

use std::sync::Arc;

use rustak_core::telemetry::Session;

use crate::config::Config;
use crate::prelude::*;

/// The `iss` claim tokens carry before the installation knows its own name.
///
/// A fresh installation configured entirely through the browser has no host
/// name until the wizard is finished, and the signing keys are loaded before
/// then. Rather than issue tokens claiming to come from an empty issuer — which
/// a later start-up would happily keep accepting — they name a URN that cannot
/// collide with any real base URL, so the sessions minted during the wizard
/// stop being accepted the moment the server learns what it is called.
const UNCONFIGURED_ISSUER: &str = "urn:rustak:unconfigured";

/// Runs the server until `shutdown` is cancelled.
///
/// `session` is the telemetry session `main` brought up before reading the
/// configuration, and is cloned into every listener and job; the caller flushes
/// it after this returns, which is why it is taken by value rather than made
/// here.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for anything the operator can fix — a
/// data directory that cannot be written, an address already in use, a TLS
/// configuration we will not serve — and a
/// [`human_errors::Kind::System`] error for a failure inside the server. Either
/// way every other component has already been asked to stop and the database
/// has been checkpointed by the time this returns.
pub async fn run(config: Config, session: Arc<Session>, shutdown: Shutdown) -> Result<(), Error> {
    install_crypto_provider();

    let context = build_context(config, session, shutdown).await?;

    runtime::run_all(context).await
}

/// Opens the storage and fills in the handles that arrive late.
///
/// Public because an integration test that drives one part of the server still
/// needs the whole of its storage, and building that by hand in a test is how a
/// test stops resembling the thing it is testing. The returned context has both
/// [`Late`](crate::services::Late) slots installed, so
/// [`Services::content`] and [`Services::jwt`] are ready to use.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the data directory, the database
/// or the encryption key cannot be opened, and a
/// [`human_errors::Kind::System`] error when the signing keys cannot be read
/// back or decrypted.
#[instrument("server.context", skip_all, err(Display))]
pub async fn build_context(
    config: Config,
    session: Arc<Session>,
    shutdown: Shutdown,
) -> Result<AppContext, Error> {
    let database_path = config.database_path();
    let content_dir = config.content_dir();

    // The database first: opening it creates the data directory, which is where
    // the generated encryption key is then written.
    let db = db::Database::open(
        &database_path,
        config.storage.reader_connections,
        busy_timeout(&config),
    )
    .await?;

    // On a blocking thread: loading the key reads a file, and creating one
    // writes and chmods a file. Start-up only today, so the impact is nil — but
    // blocking `std::fs` under an async caller is the sort of thing that gets
    // reused on a request path later, and then it is a stalled reactor.
    let secrets = {
        let configured = config.auth.secret_key.clone();
        let previous = config.auth.previous_secret_keys.clone();
        let beside = database_path.clone();

        tokio::task::spawn_blocking(move || {
            crypto::SecretStore::load(configured.as_deref(), &previous, &beside)
        })
        .await
        .or_system_err(&["Please report this issue to the development team via GitHub."])??
    };

    let context = AppContext::new(config, db, secrets, session, shutdown)?;

    let content = store::ContentStore::new(content_dir);
    content.prepare().await?;
    context.install_content(Arc::new(content))?;

    let config = context.config();
    let issuer = identity::settings::base_url(&config, context.db())
        .await?
        .unwrap_or_else(|| UNCONFIGURED_ISSUER.to_string());

    let jwt = auth::jwt::JwtIssuer::load_or_create(
        context.db(),
        context.secrets(),
        &config.auth,
        &issuer,
    )
    .await?;
    context.install_jwt(Arc::new(jwt))?;

    info!(
        database = %database_path.display(),
        readers = config.storage.reader_connections,
        "Storage is open."
    );

    Ok(context)
}

/// `[storage] busy_timeout` as the standard library spells a duration.
///
/// `chrono` durations can be negative and `std` ones cannot, so a value that
/// will not convert falls back to the database layer's own default. Refusing to
/// start over a busy timeout somebody typed a minus sign into would be a
/// disproportionate answer to a value that only decides how long a writer waits.
fn busy_timeout(config: &Config) -> std::time::Duration {
    config
        .storage
        .busy_timeout
        .to_std()
        .unwrap_or(db::connection::DEFAULT_BUSY_TIMEOUT)
}

/// Installs rustls' cryptography before anything asks for it.
///
/// Defensive rather than necessary: every rustls entry point we use would pick
/// `aws-lc-rs` on its own, but if a transitive dependency ever enables `ring`
/// as well then *neither* is the default and the first handshake panics with a
/// message about a missing `CryptoProvider`. Choosing here turns that into a
/// decision rather than a crash, and a second install — from
/// [`web::tls`], or from a test that built a listener without going through
/// start-up — is a no-op.
fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration pointing at a directory that does not exist yet, which
    /// is what a first start really looks like.
    fn config(data_dir: &std::path::Path) -> Config {
        Config::testing(data_dir.join("data"))
    }

    /// A session that records into memory.
    ///
    /// Built here rather than through `rustak_core::telemetry::testing_session`,
    /// which is behind that crate's own `testing` feature — the same trade
    /// `services::mock` makes, and for the same reason.
    fn session() -> Arc<Session> {
        Arc::new(Session::new("rustak", "0.0.0-test").with_battery(tracing_batteries::Testing))
    }

    #[tokio::test]
    async fn a_first_start_creates_everything_it_needs() {
        // The whole of `build_context` in one assertion: a data directory that
        // is not there yet ends up holding a migrated database, a generated
        // encryption key and a content store, and both late handles are filled.
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let database = config.database_path();
        let content = config.content_dir();

        let context = build_context(config, session(), Shutdown::new())
            .await
            .unwrap();

        assert!(database.exists(), "the database file should have been made");
        assert!(content.is_dir(), "the content store should be prepared");
        assert!(context.content().is_ok());
        assert!(context.jwt().is_ok());
    }

    #[tokio::test]
    async fn an_installation_that_does_not_know_its_name_yet_says_so_in_its_tokens() {
        // A session minted during the wizard must not survive the server
        // learning what host it is reached on: the issuer it names cannot be a
        // base URL, so it can never match one.
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(directory.path());
        config.server.domains = Vec::new();
        config.server.base_url = None;

        let context = build_context(config, session(), Shutdown::new())
            .await
            .unwrap();

        assert_eq!(context.jwt().unwrap().issuer(), UNCONFIGURED_ISSUER);
    }

    #[tokio::test]
    async fn a_data_directory_that_cannot_be_written_is_the_operators_to_fix() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("not-a-directory");
        std::fs::write(&file, "").unwrap();

        let mut config = Config::testing(file.join("data"));
        config.web.public.listen = Vec::new();

        let Err(err) = build_context(config, session(), Shutdown::new()).await else {
            panic!("a data directory inside a file should not open");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
    }

    #[test]
    fn installing_the_cryptography_provider_twice_is_not_a_failure() {
        // `run()` installs it and `web::tls` installs it again for the tests
        // that build a listener directly; the second call must be a no-op
        // rather than a panic.
        install_crypto_provider();
        install_crypto_provider();

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
