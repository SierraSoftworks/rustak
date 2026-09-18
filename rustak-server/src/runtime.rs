//! Everything that runs, started together and stopped together.
//!
//! [`run_all`] is the second half of start-up: the storage is already open, and
//! what is left is the things that have a lifetime — the public listener, the
//! CoT stream listener, the job host, and the housekeeping that none of them
//! owns. All four share one [`Shutdown`], so a `SIGTERM`, a failed listener and
//! a test cancelling its token are the same event as far as the rest of the
//! server is concerned.
//!
//! # Why the failures are joined rather than awaited in turn
//!
//! Each of them runs until it is told to stop, so awaiting them one after
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
//!
//! # The shutdown budget
//!
//! Stopping is two waits, and they are bounded separately because only one of
//! them is the operator's to lengthen.
//!
//! The **drain** is `[server] shutdown_timeout`, and it covers all three
//! listeners at once: the public `HttpServer`, the Marti one and the CoT
//! stream. They are all told to stop by the same cancellation and they all
//! wait for the same kind of thing — a connection somebody else has to close —
//! so one deadline for the lot is the only one an operator could reason about.
//! The wait for them is bounded here rather than in any one of them, which is
//! also what makes the actix listeners' own `shutdown_timeout` a second less
//! than it.
//!
//! The **checkpoint** afterwards is [`DATABASE_CLOSE_TIMEOUT`], a fixed two
//! seconds that is not configurable. It is not waiting for anybody else: the
//! listeners have stopped, and folding a bounded write-ahead log back into the
//! database file either takes a moment or is not going to happen at all.
//!
//! Together they are the number an orchestrator has to be told, and `docker
//! stop`'s ten-second default is exactly the 8 + 2 the defaults add up to —
//! see `docs/deployment.md` and `rustak-server/Dockerfile`.
//!
//! # A second signal ends the drain, not the process
//!
//! [`Shutdown::abort`] resolves the drain at once instead of waiting the budget
//! out, and the checkpoint still runs. The components that were still going are
//! dropped rather than awaited: they have already been told to stop, so what is
//! dropped is the waiting rather than the stopping, and the process is two
//! seconds from exiting anyway.

use std::future::Future;
use std::sync::Arc;

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

/// How long the final `TRUNCATE` checkpoint is given once everything has
/// stopped.
///
/// Fixed rather than configurable, and deliberately short. Nothing is being
/// waited *for* by then — every listener has stopped and the log is capped at
/// 64 MiB — so a checkpoint that has not finished in two seconds is one that is
/// blocked rather than slow, and the only thing left to do about it is to leave
/// the log for the next start to fold in. Two seconds is also what is left of
/// `docker stop`'s ten-second grace once the default drain has had its eight.
pub const DATABASE_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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

    close_database(&context).await;

    outcome
}

/// Folds the write-ahead log back into the database file, within the cap.
///
/// Runs on every path, including the ones where nothing ever started: a
/// listener that would not bind still leaves a migrated database with a log to
/// fold back in. It runs after a drain that was given up on, too, which is the
/// whole point of [`Shutdown::abort`] replacing the `std::process::exit` that
/// used to answer a second signal.
///
/// A failure is worth reporting but not worth replacing the failure that caused
/// the shutdown — that is the one an operator has to act on — so it is logged
/// rather than returned. Nothing is lost when it does fail: the log is still
/// there, and the next start folds it in.
async fn close_database(context: &AppContext) {
    let closed = rustak_core::runtime::with_grace(
        "the database checkpoint",
        context.db().clone().close(),
        DATABASE_CLOSE_TIMEOUT,
    )
    .await;

    match closed {
        Ok(Ok(())) => {}
        Ok(Err(err)) | Err(err) => {
            warn!(error = %err, "The database was not checkpointed cleanly on the way out.");
        }
    }
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

    // Only when something is going to ask a client for a certificate. Loading
    // the authority also issues this server's own certificate, which needs a
    // host name — and an installation with no stream listener, no Marti
    // listener, no TLS and no configured domain is a development server that
    // must still start.
    let pki = match config.stream.tls.enabled || config.web.marti.enabled {
        true => {
            let pki = mutual_tls_authority(context).await?;
            context.install_pki(Arc::clone(&pki))?;

            Some(pki)
        }
        false => None,
    };

    info!(
        version = env!("CARGO_PKG_VERSION"),
        name = %config.server.name,
        "rustak is running."
    );

    // Bound after the authority is installed, because its TLS configuration and
    // its handlers both come from it.
    let marti = crate::web::build_marti(context.clone())?;

    let shutdown = context.shutdown().clone();
    let components = (
        stopping_on_exit(&shutdown, serve(context.clone(), server)),
        stopping_on_exit(&shutdown, serve_marti(context.clone(), marti)),
        stopping_on_exit(
            &shutdown,
            crate::stream::serve(
                context.clone(),
                pki,
                crate::missions::MissionPublisher::shared(context.clone()),
            ),
        ),
        stopping_on_exit(&shutdown, jobs(context.clone())),
        stopping_on_exit(&shutdown, housekeeping(context.clone())),
    )
        .join();

    // A drain that was given up on is not an error: the server was asked to
    // stop and it has, and turning somebody's slow connection into a non-zero
    // exit status would make every restart look like a failure. The give-up is
    // logged where it happens.
    // Boxed: the five components and both `select!` arms live inside this
    // future, and `clippy::large_futures` is right that a frame that size does
    // not belong on the stack of every caller up to `main`.
    let Some((web, tak, stream, jobs, housekeeping)) = Box::pin(drain(
        &shutdown,
        components,
        config.server.shutdown_budget(),
    ))
    .await
    else {
        return Ok(());
    };

    // The first failure in start-up order, which is the one that caused the
    // shutdown; the others will be the `Ok(())` of a component that noticed the
    // cancellation and wound down.
    web.and(tak).and(stream).and(jobs).and(housekeeping)
}

/// Runs the components, and bounds the wait once one of them has to stop.
///
/// The budget starts when the shutdown is requested rather than when the server
/// starts, because until then there is nothing to bound — a listener serving
/// requests is not overrunning anything.
///
/// [`None`] when the wait was given up on, either because the budget ran out or
/// because [`Shutdown::abort`] said not to wait at all. The components are
/// dropped at that point: each of them has already been *told* to stop, so what
/// is abandoned is the waiting, and [`close_database`] runs either way.
async fn drain<F: Future>(
    shutdown: &Shutdown,
    components: F,
    budget: std::time::Duration,
) -> Option<F::Output> {
    let mut components = std::pin::pin!(components);

    tokio::select! {
        outcomes = &mut components => return Some(outcomes),
        () = shutdown.cancelled() => {}
    }

    tokio::select! {
        outcomes = &mut components => Some(outcomes),
        () = shutdown.aborted() => {
            warn!("No longer waiting for connections to close; the database is still checkpointed before we exit.");

            None
        }
        () = tokio::time::sleep(budget) => {
            warn!(
                ?budget,
                "Connections were still open when the shutdown budget ran out, so they have been cut off. Raise [server] shutdown_timeout if a clean drain needs longer, and raise the orchestrator's own grace period with it."
            );

            None
        }
    }
}

/// Runs the Marti listener, when there is one.
///
/// An installation that switched `[web.marti] enabled` off waits for the
/// shutdown instead, so that the component still exists and still ends when
/// everything else does — rather than returning at once and, through
/// [`stopping_on_exit`], stopping the whole server.
async fn serve_marti(
    context: AppContext,
    server: Option<actix_web::dev::Server>,
) -> Result<(), Error> {
    match server {
        Some(server) => serve_named(context, server, "Marti").await,
        None => {
            context.shutdown().cancelled().await;

            Ok(())
        }
    }
}

/// Loads the authority every mutually authenticated listener is built from.
///
/// Separate from the CA material the public listener's own certificate is
/// issued from, because they want different things from it: that wants signing
/// material, and this wants a trust anchor plus the revocation cache that
/// decides whether a certificate still counts. Installed on the context as well
/// as handed to the stream, so that the enrolment endpoints reach the same
/// authority and the same cache.
async fn mutual_tls_authority(context: &AppContext) -> Result<Arc<crate::pki::Pki>, Error> {
    let config = context.config();
    let stored = crate::identity::settings::stored(context.db())
        .await?
        .domains;
    let names = crate::web::tls::server_names(&config, &stored);

    crate::pki::Pki::load(
        context.db(),
        context.secrets(),
        &config.pki,
        &config.server.data_dir,
        &names,
        &config.pki.server_ips,
    )
    .await
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
async fn serve(context: AppContext, server: actix_web::dev::Server) -> Result<(), Error> {
    serve_named(context, server, "public").await
}

/// Runs one actix listener, stopping it when the token is cancelled.
///
/// actix's `Server` future resolves when the server has stopped, and the only
/// way to ask it to stop is `ServerHandle::stop` from somewhere else — hence the
/// handle taken before the future is awaited. There is no timeout here: actix's
/// own `shutdown_timeout` bounds the drain, [`drain`] bounds the wait for it a
/// second later, and a third deadline in between would only decide which of the
/// two got to log it.
async fn serve_named(
    context: AppContext,
    server: actix_web::dev::Server,
    what: &'static str,
) -> Result<(), Error> {
    let handle = server.handle();
    let shutdown = context.shutdown().clone();

    let stopper = tokio::spawn(async move {
        shutdown.cancelled().await;
        info!("Draining the {what} listener.");
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

    info!("The {what} listener has stopped.");

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

/// Deletes half-finished ceremonies and flows nobody came back to.
///
/// Housekeeping rather than correctness: an expired passkey ceremony, an
/// abandoned sign-in and a code nobody redeemed are each refused on their own
/// merits whether or not they have been swept. What this stops is the tables
/// growing without limit on a server people keep closing tabs on.
async fn sweep_ceremonies(context: &AppContext) {
    match crate::auth::passkey_store::sweep(context.db()).await {
        Ok(0) => {}
        Ok(removed) => debug!(removed, "Swept expired passkey ceremonies."),
        Err(err) => warn!(error = %err, "Could not sweep expired passkey ceremonies."),
    }

    match crate::auth::oauth_server::state::sweep(context.db()).await {
        Ok(0) => {}
        Ok(removed) => debug!(removed, "Swept abandoned sign-ins."),
        Err(err) => warn!(error = %err, "Could not sweep abandoned sign-ins."),
    }

    match crate::auth::oauth_server::codes::prune(context.db(), chrono::Utc::now()).await {
        Ok(0) => {}
        Ok(removed) => debug!(removed, "Pruned expired authorization codes."),
        Err(err) => warn!(error = %err, "Could not prune expired authorization codes."),
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
    async fn everything_stopping_on_its_own_is_reported_rather_than_waited_out() {
        // The ordinary path: the components wound down, and what they returned
        // is what `listen` goes on to fold into one result.
        let shutdown = Shutdown::new();

        let finished = drain(&shutdown, async { 7 }, std::time::Duration::from_secs(8)).await;

        assert_eq!(finished, Some(7));
    }

    #[tokio::test]
    async fn a_drain_that_outlives_its_budget_is_given_up_on() {
        // What used to be actix's ten seconds and Docker's ten seconds racing
        // each other: the wait has to end on *our* deadline, with the
        // checkpoint still to come, rather than on the orchestrator's SIGKILL.
        let shutdown = Shutdown::new();
        shutdown.cancel();

        let abandoned = drain(
            &shutdown,
            std::future::pending::<()>(),
            // Short, because what is being tested is that the budget is
            // enforced rather than how long the default one is.
            std::time::Duration::from_millis(50),
        )
        .await;

        assert!(abandoned.is_none(), "a drain that never ends has to be cut");
    }

    #[tokio::test]
    async fn a_second_signal_ends_the_wait_without_waiting_the_budget_out() {
        // The operator pressing Ctrl-C twice: they are answered now, not in ten
        // minutes, and `run_all` still gets to run the checkpoint afterwards.
        let shutdown = Shutdown::new();
        shutdown.abort();

        let abandoned = rustak_core::runtime::with_grace(
            "an aborted drain",
            drain(
                &shutdown,
                std::future::pending::<()>(),
                std::time::Duration::from_secs(600),
            ),
            std::time::Duration::from_secs(1),
        )
        .await
        .expect("an abort does not wait for the budget");

        assert!(abandoned.is_none());
    }

    #[tokio::test]
    async fn the_checkpoint_runs_after_a_drain_that_was_abandoned() {
        // The whole reason the second signal no longer calls `process::exit`.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        context.shutdown().abort();

        rustak_core::runtime::with_grace(
            "the checkpoint",
            close_database(&context),
            DATABASE_CLOSE_TIMEOUT + std::time::Duration::from_secs(1),
        )
        .await
        .expect("the checkpoint is bounded by its own cap");
    }

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
