//! Everything that runs, started together and stopped together.
//!
//! [`run_all`] is the second half of start-up: the storage is already open, and
//! what is left is the things that have a lifetime — the public listener, the
//! job host, and the housekeeping that neither of them owns. All three share
//! one [`Shutdown`], so a `SIGTERM`, a failed listener and a test cancelling its
//! token are the same event as far as the rest of the server is concerned.
//!
//! # Why the failures are joined rather than awaited in turn
//!
//! Each of the three runs until it is told to stop, so awaiting them one after
//! another would mean never noticing that the second had died. They are joined,
//! and the first failure cancels the token — which is what stops the other two —
//! so the error that comes back is the one that caused the shutdown rather than
//! the `Ok(())` of whichever component noticed the cancellation first.
//!
//! # Why the database is closed here
//!
//! Under WAL a commit appends to `rustak.sqlite-wal` and the main file catches
//! up at a checkpoint. [`Database::close`](crate::db::Database::close) runs a
//! `TRUNCATE` checkpoint, so the data directory a stopped server leaves behind
//! is the database and an empty log rather than a database and megabytes of
//! pending writes. It runs after everything else has stopped, and on every exit
//! path — a listener that would not bind is no reason to leave the log
//! unfolded, since the migrations that ran before it already wrote to it.

use std::future::Future;

use futures_concurrency::future::Join as _;

use crate::auth::setup::SetupToken;
use crate::jobs::JobHost;
use crate::prelude::*;

/// How often the short-lived authentication state is swept.
///
/// None of it is load-bearing: an expired ceremony is refused whether or not it
/// has been deleted, and a revoked token is checked against the row rather than
/// against its absence. This is housekeeping, so the cadence only decides how
/// much dead state an installation carries between sweeps.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

/// How often revoked access tokens that have since expired are dropped.
const JTI_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Starts every listener and background task, and runs until shutdown.
///
/// Returns `Ok(())` when the [`Shutdown`] in `context` was cancelled and
/// everything wound down; the first failure otherwise, having cancelled the
/// token on the way out so nothing is left running.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for the things an operator configured —
/// an address that will not bind, a TLS mode we refuse to serve — and a
/// [`human_errors::Kind::System`] error for a failure inside the server.
#[instrument("server.run", skip_all, err(Display))]
pub async fn run_all(context: AppContext) -> Result<(), Error> {
    let outcome = listen(&context).await;

    // On every path, including the ones where nothing ever started: a listener
    // that would not bind still leaves a migrated database with a log to fold
    // back in. A failure here is worth reporting but not worth replacing the
    // failure that caused the shutdown, which is the one an operator has to act
    // on — so it is logged rather than returned.
    if let Err(err) = context.db().clone().close().await {
        warn!(error = %err, "The database was not checkpointed cleanly on the way out.");
    }

    outcome
}

/// Everything [`run_all`] does before the database is closed.
async fn listen(context: &AppContext) -> Result<(), Error> {
    let config = context.config();

    // The authority first: the internal TLS mode issues the listener's
    // certificate from it, and loading it also refreshes `<data_dir>/pki/ca.crt`
    // so that a device enrolling later has something to trust.
    let ca = crate::pki::load_or_create_root_ca(
        context.db(),
        context.secrets(),
        &config.pki,
        &config.server.data_dir,
    )
    .await?;

    announce_setup(context).await?;

    let tls = crate::web::tls::resolve(&config, context.db(), context.secrets(), Some(&ca)).await?;
    let server = crate::web::build_public(context.clone(), tls)?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        name = %config.server.name,
        "rustak is running."
    );

    let shutdown = context.shutdown().clone();
    let (web, jobs, housekeeping) = (
        stopping_on_exit(&shutdown, serve(context.clone(), server)),
        stopping_on_exit(&shutdown, jobs(context.clone())),
        stopping_on_exit(&shutdown, housekeeping(context.clone())),
    )
        .join()
        .await;

    // The first failure in start-up order, which is the one that caused the
    // shutdown; the others will be the `Ok(())` of a component that noticed the
    // cancellation and wound down.
    web.and(jobs).and(housekeeping)
}

/// Runs `future`, and cancels the shared token however it ends.
///
/// This is what makes the three components one server rather than three. A
/// failure in any of them stops the other two, and so does a clean return that
/// nobody asked for — a listener whose last socket closed is a process that
/// answers nothing, and carrying on would leave an operator with a server that
/// is running and useless.
async fn stopping_on_exit<F>(shutdown: &Shutdown, future: F) -> Result<(), Error>
where
    F: Future<Output = Result<(), Error>>,
{
    let outcome = future.await;
    shutdown.cancel();
    outcome
}

/// Runs the public listener, stopping it when the token is cancelled.
///
/// actix's `Server` future resolves when the server has stopped, and the only
/// way to ask it to stop is `ServerHandle::stop` from somewhere else — hence the
/// handle taken before the future is awaited. The drain is bounded by the
/// `shutdown_timeout` `web::server` sets, so there is no second timeout here.
async fn serve(context: AppContext, server: actix_web::dev::Server) -> Result<(), Error> {
    let handle = server.handle();
    let shutdown = context.shutdown().clone();

    let stopper = tokio::spawn(async move {
        shutdown.cancelled().await;
        info!("Draining the public listener.");
        // `true`: wait for in-flight requests rather than cutting them off. A
        // request being served when the signal arrives is somebody's upload.
        handle.stop(true).await;
    });

    let served = server.await;
    stopper.abort();

    served.or_system_err(&[
        "This usually means a listener socket failed while the server was running.",
        "Please report this issue to the development team via GitHub.",
    ])?;

    info!("The public listener has stopped.");

    Ok(())
}

/// Runs the job host, which stops itself when the token is cancelled.
///
/// An error from it means a job could not be set up, which leaves housekeeping
/// silently not running — a reason to stop the server rather than to carry on
/// without it.
async fn jobs(context: AppContext) -> Result<(), Error> {
    JobHost::run(context).await
}

/// The sweeps nothing else owns.
///
/// Not queue jobs, unlike the audit prune and the WAL checkpoint, because
/// neither of these is durable work: they delete rows that are already expired,
/// and a missed sweep costs nothing. Putting them in the queue would mean
/// writing a message to the database every ten minutes for the privilege.
async fn housekeeping(context: AppContext) -> Result<(), Error> {
    let shutdown = context.shutdown().clone();
    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    let mut prune = tokio::time::interval(JTI_PRUNE_INTERVAL);

    // The first tick of a tokio interval is immediate, and there is nothing to
    // sweep at start-up; skipping it keeps two queries off the critical path.
    sweep.tick().await;
    prune.tick().await;

    loop {
        tokio::select! {
            // Biased so that a server being stopped never takes one more sweep
            // because two branches happened to be ready at once.
            biased;

            () = shutdown.cancelled() => break,
            _ = sweep.tick() => sweep_ceremonies(&context).await,
            _ = prune.tick() => prune_revocations(&context).await,
        }
    }

    Ok(())
}

/// Deletes passkey ceremonies nobody came back to finish.
async fn sweep_ceremonies(context: &AppContext) {
    match crate::auth::passkey_store::sweep(context.db()).await {
        Ok(0) => {}
        Ok(removed) => debug!(removed, "Swept expired passkey ceremonies."),
        Err(err) => warn!(error = %err, "Could not sweep expired passkey ceremonies."),
    }
}

/// Drops revocations for tokens that have expired anyway.
async fn prune_revocations(context: &AppContext) {
    match context.db().revoked_jtis().prune().await {
        Ok(0) => {}
        Ok(removed) => debug!(removed, "Pruned expired token revocations."),
        Err(err) => warn!(error = %err, "Could not prune expired token revocations."),
    }
}

/// Writes the first-run setup token, and says where it went.
///
/// The token itself is never logged: it is the credential that creates the
/// first administrator, and a log line carrying it would put it wherever the
/// logs go. The path is logged, because somebody who can read the file can read
/// the token and somebody who cannot has learnt nothing.
async fn announce_setup(context: &AppContext) -> Result<(), Error> {
    let config = context.config();
    let token = crate::auth::setup::ensure(context.db(), &config.setup_token_file()).await?;

    let Some(SetupToken { path, .. }) = token else {
        return Ok(());
    };

    let where_to_go = config
        .server
        .base_url()
        .map_or_else(|| "/setup".to_string(), |base| format!("{base}/setup"));

    warn!(
        token_file = %path.display(),
        "Setup required: open {where_to_go} and enter the token from {}.",
        path.display(),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn housekeeping_stops_when_the_server_does() {
        // The property every branch of the loop depends on: a cancellation ends
        // it at the next turn rather than at the next interval, which for the
        // hourly prune would be an hour.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        context.shutdown().cancel();

        rustak_core::runtime::with_grace(
            "housekeeping",
            housekeeping(context),
            std::time::Duration::from_secs(1),
        )
        .await
        .expect("a cancelled housekeeping loop returns at once")
        .unwrap();
    }

    #[tokio::test]
    async fn a_sweep_that_fails_is_logged_rather_than_fatal() {
        // Housekeeping is not load-bearing — an expired ceremony is refused
        // whether or not it was swept — so a database that is briefly unhappy
        // must not be the reason the server stops answering requests.
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        sweep_ceremonies(&context).await;
        prune_revocations(&context).await;
    }

    #[tokio::test]
    async fn a_fresh_installation_is_told_how_to_get_in() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let context = AppContext::new_mock(move |config| {
            *config = crate::config::Config::testing(path);
        })
        .await
        .unwrap();

        announce_setup(&context).await.unwrap();

        let token_file = context.config().setup_token_file();
        assert!(
            token_file.is_file(),
            "a fresh installation writes the token somewhere its operator can read it",
        );
    }

    #[tokio::test]
    async fn an_installation_that_has_been_set_up_is_not_told_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let context = AppContext::new_mock(move |config| {
            *config = crate::config::Config::testing(path);
        })
        .await
        .unwrap();

        crate::identity::settings::complete(context.db(), None)
            .await
            .unwrap();

        announce_setup(&context).await.unwrap();

        assert!(
            !context.config().setup_token_file().exists(),
            "a completed installation must not mint a token that creates an administrator",
        );
    }
}
