//! Stopping cleanly: one cancellation signal shared by every listener and job.
//!
//! A rustak server holds long-lived things — TLS stream connections that may be
//! idle for ninety seconds at a time, an HTTP server, a job host, a WAL
//! checkpoint timer. Stopping means telling all of them at once and then waiting
//! for each to finish what it was doing, which is exactly what a
//! [`CancellationToken`] is for. [`Shutdown`] is that token plus the two things
//! every binary needs around it: signal handling that does the right thing when
//! an impatient operator presses Ctrl-C twice, and a bounded wait
//! ([`with_grace`]) for work that may not stop on its own.
//!
//! # What the second signal does
//!
//! The first `SIGINT` or `SIGTERM` starts a graceful shutdown: connections are
//! drained, in-flight requests finish, the database is checkpointed. That takes
//! a moment, and an operator watching a container that appears not to be
//! stopping will press Ctrl-C again. Ignoring them would teach them to reach
//! for `kill -9`, so the second signal is honoured — but it ends the *drain*
//! rather than the process. [`Shutdown::abort`] fires, whoever was waiting on
//! connections that are still open stops waiting, and the caller still gets to
//! run the few things that have to happen before the process goes away.
//!
//! For rustak that is `Database::close`, whose `TRUNCATE` checkpoint decides
//! whether the next start has a write-ahead log to fold back in. Exiting from
//! the signal handler instead — which is what this module used to do — skipped
//! it, so the impatient operator was the one who got the untidy data directory.
//!
//! A third signal is the escape hatch and exits at once with the conventional
//! `128 + SIGINT` status. It is the only path that skips the checkpoint, and
//! somebody who has asked three times has said what they mean.

use std::future::Future;

use tokio_util::sync::CancellationToken;

/// The exit status of a process ended by a second interrupt: `128 + SIGINT`.
const EXIT_INTERRUPTED: i32 = 130;

/// The shared "stop now" signal, cloned into every listener, connection and job.
///
/// Cloning is cheap and every clone observes the same cancellation. A
/// [`child`](Shutdown::child) is cancelled when its parent is, but cancelling it
/// does not stop the rest of the server — which is how one listener can fail and
/// shut only itself down.
///
/// ```
/// # use rustak_core::runtime::Shutdown;
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let shutdown = Shutdown::new();
/// let listener = shutdown.clone();
///
/// assert!(!listener.is_cancelled());
/// shutdown.cancel();
///
/// // Every clone sees it, and `cancelled()` returns immediately afterwards.
/// listener.cancelled().await;
/// assert!(listener.is_cancelled());
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    /// "Stop when you can": scoped, so a [`child`](Shutdown::child) can be
    /// stopped without stopping the rest of the server.
    stop: CancellationToken,
    /// "Stop waiting": shared by every clone *and* every child, because running
    /// out of time is a fact about the process rather than about one listener.
    abort: CancellationToken,
}

impl Shutdown {
    /// A fresh, uncancelled signal. One per process, cloned from there.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns the task that turns `SIGINT`/`SIGTERM` into a cancellation.
    ///
    /// The first signal cancels this token, the second
    /// [abandons](Shutdown::abort) the drain, and a third ends the process at
    /// once with status 130 — see the [module documentation](self).
    ///
    /// Call this once, from `main`, after the runtime is up. On platforms
    /// without `SIGTERM` only Ctrl-C is listened for.
    pub fn listen_for_signals(&self) {
        let (signals, received) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            loop {
                next_signal().await;

                // The sequencer stops receiving once it has seen its third, at
                // which point there is nothing left to tell it.
                if signals.send(()).await.is_err() {
                    break;
                }
            }
        });

        self.sequence(received);
    }

    /// The sequencing itself, over a channel rather than over the signal
    /// handlers, so that a test can deliver the second signal without sending
    /// one to the process running it.
    fn sequence(&self, mut signals: tokio::sync::mpsc::Receiver<()>) {
        let shutdown = self.clone();

        tokio::spawn(async move {
            if signals.recv().await.is_none() {
                return;
            }

            tracing::info!("Received a shutdown signal; draining connections.");
            shutdown.cancel();

            if signals.recv().await.is_none() {
                return;
            }

            tracing::warn!(
                "Received a second shutdown signal; connections that are still open will be cut off. The database is still checkpointed before we exit."
            );
            shutdown.abort();

            if signals.recv().await.is_none() {
                return;
            }

            tracing::warn!(
                "Received a third shutdown signal; exiting immediately. The write-ahead log will be folded back in on the next start."
            );
            std::process::exit(EXIT_INTERRUPTED);
        });
    }

    /// Resolves once shutdown has been requested, and immediately thereafter.
    ///
    /// This is the branch to `select!` against in any loop that should stop.
    pub async fn cancelled(&self) {
        self.stop.cancelled().await;
    }

    /// Requests shutdown. Idempotent, and visible to every clone and child.
    pub fn cancel(&self) {
        self.stop.cancel();
    }

    /// Whether shutdown has already been requested.
    pub fn is_cancelled(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// Gives up on the drain: whatever is still open will not be waited for.
    ///
    /// Implies [`cancel`](Shutdown::cancel), so it is also a way to stop a
    /// server that had not been asked to stop yet. Every clone *and* every
    /// child sees it, unlike the stop signal — a listener cannot decide on its
    /// own that the process has run out of time.
    pub fn abort(&self) {
        self.stop.cancel();
        self.abort.cancel();
    }

    /// Resolves once the drain has been given up on.
    ///
    /// The branch to `select!` against wherever a bounded wait for connections
    /// to close happens, so that an operator who asks twice is answered at once
    /// rather than at the end of the budget.
    pub async fn aborted(&self) {
        self.abort.cancelled().await;
    }

    /// Whether the drain has been given up on.
    pub fn is_aborted(&self) -> bool {
        self.abort.is_cancelled()
    }

    /// A signal that stops when this one does, but whose own cancellation stays
    /// local — one listener, one connection, one job.
    ///
    /// The abort is shared rather than scoped: a child that outlived the
    /// process's patience is exactly the thing the abort exists to stop
    /// waiting for.
    pub fn child(&self) -> Self {
        Self {
            stop: self.stop.child_token(),
            abort: self.abort.clone(),
        }
    }

    /// The underlying stop token, for the APIs that take one directly
    /// (`CancellationToken::run_until_cancelled`, `DropGuard`, and so on).
    pub fn token(&self) -> &CancellationToken {
        &self.stop
    }
}

/// Runs `future` to completion, or gives up after `timeout`.
///
/// This is the wait that follows a cancellation: everything has been *told* to
/// stop, and this bounds how long we let it take. A component that overruns is a
/// bug in that component, so the failure is a
/// [`Kind::System`](human_errors::Kind::System) error rather than a user one —
/// but the process still exits, because a shutdown that can be blocked
/// indefinitely is a shutdown that ends in `kill -9`.
///
/// # Errors
///
/// Returns a [`Kind::System`](human_errors::Kind::System) error naming `what`
/// when the future has not completed within `timeout`.
pub async fn with_grace<F: Future>(
    what: &'static str,
    future: F,
    timeout: std::time::Duration,
) -> Result<F::Output, human_errors::Error> {
    match tokio::time::timeout(timeout, future).await {
        Ok(output) => Ok(output),
        Err(_) => Err(human_errors::system(
            format!("{what} did not finish shutting down within {timeout:?}."),
            crate::errors::ADVICE_REPORT_DEV,
        )),
    }
}

/// Resolves on the next `SIGINT`, or `SIGTERM` where there is one.
///
/// Installing the `SIGTERM` handler can fail (it needs the runtime's signal
/// driver); if it does we fall back to Ctrl-C alone rather than refusing to
/// start, because a server that will not boot is worse than one that only knows
/// about one of the two signals.
async fn next_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "Could not listen for SIGTERM; only Ctrl-C will stop rustak gracefully.",
                );
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_clone_sees_the_same_cancellation() {
        // The property the whole type exists for: one `cancel()` stops the HTTP
        // server, the stream listener, the job host and every open connection.
        let shutdown = Shutdown::new();
        let listener = shutdown.clone();
        let job = shutdown.clone();

        assert!(!listener.is_cancelled());
        shutdown.cancel();

        listener.cancelled().await;
        job.cancelled().await;
        assert!(listener.is_cancelled() && job.is_cancelled());
    }

    #[tokio::test]
    async fn cancelling_after_the_fact_still_resolves_immediately() {
        // A task that starts *after* shutdown was requested must not wait for a
        // signal that has already been given — otherwise a connection accepted
        // during the drain would hang the drain.
        let shutdown = Shutdown::new();
        shutdown.cancel();

        with_grace(
            "a late waiter",
            shutdown.cancelled(),
            std::time::Duration::from_secs(1),
        )
        .await
        .expect("an already-cancelled token resolves at once");
    }

    #[tokio::test]
    async fn a_child_stops_when_its_parent_does() {
        let shutdown = Shutdown::new();
        let connection = shutdown.child();

        shutdown.cancel();

        connection.cancelled().await;
        assert!(connection.is_cancelled());
    }

    #[tokio::test]
    async fn cancelling_a_child_leaves_the_rest_of_the_server_running() {
        // This is what makes `child()` worth having: a single failed listener
        // can shut itself down without taking the others with it.
        let shutdown = Shutdown::new();
        let listener = shutdown.child();
        let other = shutdown.child();

        listener.cancel();

        assert!(listener.is_cancelled());
        assert!(!other.is_cancelled());
        assert!(!shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn a_child_shares_the_abort_even_though_it_has_its_own_stop() {
        // Running out of time is a fact about the process: a connection that is
        // still open when the operator asks a second time has to find out, and
        // its own token is a child of the server's.
        let shutdown = Shutdown::new();
        let connection = shutdown.child();

        shutdown.abort();

        assert!(connection.is_aborted());
        assert!(connection.is_cancelled(), "an abort implies a stop");
    }

    #[tokio::test]
    async fn aborting_a_child_leaves_the_process_alone() {
        // The other direction: one listener giving up on its own connections
        // must not tell the rest of the server that the budget is spent.
        let shutdown = Shutdown::new();
        let listener = shutdown.child();

        listener.cancel();

        assert!(!shutdown.is_cancelled());
        assert!(!shutdown.is_aborted());
    }

    #[tokio::test]
    async fn the_first_signal_drains_and_the_second_stops_waiting() {
        // The sequencing the whole module exists for, driven by a channel
        // rather than by `kill`: a test that sent itself a real SIGTERM would
        // be testing the harness as much as the code, and a second real one
        // used to end the process running it.
        let shutdown = Shutdown::new();
        let (signals, received) = tokio::sync::mpsc::channel(4);
        shutdown.sequence(received);

        signals.send(()).await.unwrap();
        with_grace(
            "the first signal",
            shutdown.cancelled(),
            std::time::Duration::from_secs(1),
        )
        .await
        .expect("the first signal starts the drain");
        assert!(
            !shutdown.is_aborted(),
            "one signal asks for a drain, not for the end of one",
        );

        signals.send(()).await.unwrap();
        with_grace(
            "the second signal",
            shutdown.aborted(),
            std::time::Duration::from_secs(1),
        )
        .await
        .expect("the second signal ends the wait");

        // Deliberately not a third: that one calls `std::process::exit`, which
        // would take the test binary with it.
    }

    #[tokio::test]
    async fn work_that_finishes_in_time_returns_its_value() {
        let finished = with_grace(
            "a prompt component",
            async { "done" },
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(finished, "done");
    }

    #[tokio::test]
    async fn work_that_overruns_is_reported_as_our_bug_rather_than_the_operators() {
        // A component that will not stop is not something an operator can fix
        // from a config file, so the error has to be one we hear about.
        let Err(err) = with_grace(
            "the stream listener",
            std::future::pending::<()>(),
            // Short, because the point is the refusal rather than the wait.
            std::time::Duration::from_millis(50),
        )
        .await
        else {
            panic!("a future that never completes should not pass the grace period");
        };

        assert!(err.is(human_errors::Kind::System), "{err}");
        assert!(err.to_string().contains("the stream listener"), "{err}");
    }
}
