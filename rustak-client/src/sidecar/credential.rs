//! Whether this sidecar's credential is one the server will take, and what an
//! operator should be told about it.
//!
//! # A refused credential is not an outage, but it must still back off
//!
//! [`LinkHealth`](super::link_health::LinkHealth) draws one distinction — the
//! server answered, or it did not — and a refusal leaves the link *up*, which
//! is right: a `400` on the token exchange has nothing to do with whether the
//! heartbeat route is reachable. What that left behind is the other half of the
//! problem. Because a refusal resets the link's backoff, a credential the
//! server will never accept was re-presented **every tick, forever**: the first
//! live deployment spent 45 minutes exchanging an expired assertion every five
//! seconds, 137 server log lines an hour, and then earned itself a `429` that
//! made the *next* process's first exchange fail too.
//!
//! So a refused credential has a state of its own, with the same cadence as an
//! outage — one warning, `debug` repeats, a counted reminder every five
//! minutes, one line when it works again — and a wait that widens from
//! [`RETRY_MIN`] to [`RETRY_MAX`] instead of a retry per tick.
//!
//! # Except when the credential changes
//!
//! The whole point of a workload identity is that the orchestrator replaces it.
//! A renewal that lands while we are waiting out a five-minute backoff is a
//! *different* credential, and making it serve the old one's sentence would
//! turn a one-second recovery into a five-minute one. [`Credential::changed`]
//! is that escape hatch: the token's content is fingerprinted — never logged,
//! never stored — and a fingerprint that differs from the refused one resets
//! everything.

use std::hash::{Hash as _, Hasher as _};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::link_health::{REMIND_EVERY, elapsed, humanised};

/// How long after a refusal the next exchange is worth making.
///
/// Five seconds rather than the link's one: a credential the server has just
/// refused will not be accepted a second later, and the *reason* it was refused
/// usually takes an orchestrator's renewal cycle to change.
pub(crate) const RETRY_MIN: Duration = Duration::from_secs(5);

/// The longest that wait becomes, however long it has been failing.
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(300);

/// What one refusal is worth saying out loud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The first refusal of a run — one warning, with the cause and the advice.
    First { retry_in: Duration },

    /// Nothing new: `debug`, and nothing else.
    Quiet,

    /// Still refused. One line: how long, how many attempts, and the last
    /// reason.
    Reminder {
        refused_for: Duration,
        attempts: u32,
    },
}

/// What an accepted credential is worth saying out loud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Accepted {
    /// It was working before, so this is not news.
    Quiet,

    /// It was being refused, and now it is not.
    Again { refused_for: Duration },
}

/// The pure half: a refused credential as it looks over time.
///
/// Every method takes the instant to judge against, so the tests move the clock
/// by hand and nothing here waits.
#[derive(Debug, Default)]
struct State {
    /// When the current run of refusals started; [`None`] while it works.
    refused_since: Option<DateTime<Utc>>,

    /// When this run was last mentioned at `warn`.
    mentioned_at: Option<DateTime<Utc>>,

    /// How many exchanges have been refused in this run, for the reminder.
    attempts: u32,

    /// The earliest instant another exchange is worth making.
    retry_at: Option<DateTime<Utc>>,

    /// A fingerprint of the credential that was refused — never the credential.
    presented: Option<u64>,
}

impl State {
    /// Records a refusal and answers what to say about it.
    fn refused(&mut self, mark: Option<u64>, now: DateTime<Utc>) -> Refused {
        self.attempts = self.attempts.saturating_add(1);
        self.presented = mark;

        let wait = backoff(self.attempts);
        self.retry_at = Some(now + wait);

        let Some(since) = self.refused_since else {
            self.refused_since = Some(now);
            self.mentioned_at = Some(now);

            return Refused::First { retry_in: wait };
        };

        let quiet_for = self
            .mentioned_at
            .map_or(REMIND_EVERY, |at| elapsed(at, now));

        if quiet_for < REMIND_EVERY {
            return Refused::Quiet;
        }

        self.mentioned_at = Some(now);

        Refused::Reminder {
            refused_for: elapsed(since, now),
            attempts: self.attempts,
        }
    }

    /// Forgets the run, answering what that is worth saying.
    fn accepted(&mut self, now: DateTime<Utc>) -> Accepted {
        let refused_since = self.refused_since.take();
        *self = Self::default();

        match refused_since {
            Some(since) => Accepted::Again {
                refused_for: elapsed(since, now),
            },
            None => Accepted::Quiet,
        }
    }
}

/// How long to wait after `attempts` consecutive refusals.
///
/// Capped exponential: 5s, 10s, 20s … 5m. `saturating_sub` and the cap keep the
/// shift away from overflowing however long a deployment stays broken.
fn backoff(attempts: u32) -> Duration {
    let doublings = attempts.saturating_sub(1).min(16);

    RETRY_MIN.saturating_mul(1u32 << doublings).min(RETRY_MAX)
}

/// A fingerprint of a credential, for noticing that it has been replaced.
///
/// A hash and never the token: this is stored, compared and — through
/// [`Credential`]'s `Debug` — printable, and none of those may be true of a
/// credential.
pub(crate) fn fingerprint(token: &Secret) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.expose().hash(&mut hasher);

    hasher.finish()
}

/// Whether the server is taking this sidecar's credential, shared by everything
/// that needs one.
///
/// One credential, one backoff: the heartbeat and the event feed both want a
/// token, and two of them asking must not be two exchanges — nor two warnings.
#[derive(Debug, Default)]
pub(crate) struct Credential {
    state: Mutex<State>,
}

impl Credential {
    /// A credential nothing has been said about yet, which is assumed good.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether an exchange is worth making now.
    pub(crate) fn due_at(&self, now: DateTime<Utc>) -> bool {
        self.with(|state| state.retry_at.is_none_or(|at| now >= at))
    }

    /// How long until the next exchange is worth making.
    pub(crate) fn retry_in_at(&self, now: DateTime<Utc>) -> Duration {
        self.with(|state| state.retry_at.map_or(Duration::ZERO, |at| elapsed(now, at)))
    }

    /// Records a refusal of `mark` and answers what to say about it.
    pub(crate) fn refused_at(&self, mark: Option<u64>, now: DateTime<Utc>) -> Refused {
        self.with(|state| state.refused(mark, now))
    }

    /// Records an acceptance and answers what to say about it.
    pub(crate) fn accepted_at(&self, now: DateTime<Utc>) -> Accepted {
        self.with(|state| state.accepted(now))
    }

    /// Whether `mark` is a different credential from the one that was refused,
    /// forgetting the refusal when it is.
    ///
    /// The renewal escape hatch: see the [module documentation](self).
    pub(crate) fn changed(&self, mark: u64) -> bool {
        self.with(|state| match state.presented {
            Some(refused) if refused == mark => false,
            None => false,
            Some(_) => {
                state.retry_at = None;
                state.attempts = 0;
                state.presented = Some(mark);

                true
            }
        })
    }

    /// Runs `act` against the state, taking a poisoned lock's contents rather
    /// than panicking: a credential's history is not worth failing a sidecar
    /// over.
    fn with<T>(&self, act: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        act(&mut state)
    }
}

/// Logs one refusal at the level this run of them calls for.
///
/// `answered` is whether the server is the thing that refused it. A credential
/// this sidecar could not even read never reached the server, and saying "the
/// server answered, so the link is up" about one would be a sentence that sends
/// an operator to the wrong end of the problem.
pub(crate) fn announce_refusal(report: Refused, answered: bool, err: &Error) {
    match report {
        Refused::First { retry_in } => tracing::warn!(
            error = %err,
            "Could not exchange this sidecar's workload identity for an access token. {} The next attempt is in {}, widening to {}; repeats are logged at debug until this changes.",
            whose_fault(answered),
            humanised(retry_in),
            humanised(RETRY_MAX),
        ),
        Refused::Quiet => tracing::debug!(
            error = %err.description(),
            "This sidecar's workload identity was refused again; nothing has changed since this was last reported.",
        ),
        Refused::Reminder {
            refused_for,
            attempts,
        } => tracing::warn!(
            attempts,
            error = %err.description(),
            "This sidecar's workload identity has been refused for {} over {attempts} attempts.",
            humanised(refused_for),
        ),
    }
}

/// Which end of the problem the first line should point at.
fn whose_fault(answered: bool) -> &'static str {
    match answered {
        true => {
            "The server answered, so the control link is up; it is the credential that was refused."
        }
        false => "There was no credential to present, so nothing was asked of the server.",
    }
}

/// Says so, once, when the credential starts working again.
pub(crate) fn announce_acceptance(report: Accepted) {
    if let Accepted::Again { refused_for } = report {
        tracing::info!(
            "The server accepted this sidecar's workload identity again after {}.",
            humanised(refused_for),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    #[test]
    fn the_first_refusal_is_announced_and_the_wait_starts_at_five_seconds() {
        let credential = Credential::new();

        assert!(credential.due_at(at(0)), "a credential nobody refused");
        assert_eq!(
            credential.refused_at(Some(1), at(0)),
            Refused::First {
                retry_in: RETRY_MIN
            },
        );
        assert!(!credential.due_at(at(4)));
        assert!(credential.due_at(at(5)));
    }

    #[test]
    fn the_wait_doubles_to_five_minutes_and_stops_there() {
        // The production finding: a refusal reset the link's backoff, so an
        // expired assertion was presented every five seconds for 45 minutes.
        let credential = Credential::new();

        for (attempt, expected) in [
            (1, 5),
            (2, 10),
            (3, 20),
            (4, 40),
            (5, 80),
            (6, 160),
            (7, 300),
            (8, 300),
        ] {
            credential.refused_at(Some(1), at(attempt));

            assert_eq!(
                credential.retry_in_at(at(attempt)),
                Duration::from_secs(expected),
                "after {attempt} refusals",
            );
        }

        // However long it runs, and without overflowing the shift.
        for attempt in 9..80 {
            credential.refused_at(Some(1), at(attempt));
        }

        assert_eq!(credential.retry_in_at(at(79)), RETRY_MAX);
    }

    #[test]
    fn every_refusal_after_the_first_is_quiet_until_the_reminder_falls_due() {
        let credential = Credential::new();

        assert!(matches!(
            credential.refused_at(Some(1), at(0)),
            Refused::First { .. }
        ));

        for second in [5, 15, 35, 75, 155, 299] {
            assert_eq!(
                credential.refused_at(Some(1), at(second)),
                Refused::Quiet,
                "the refusal was announced at 0s; {second}s must be quiet",
            );
        }

        assert_eq!(
            credential.refused_at(Some(1), at(300)),
            Refused::Reminder {
                refused_for: Duration::from_secs(300),
                attempts: 8,
            },
            "and the reminder counts the attempts it stayed quiet about",
        );
    }

    #[test]
    fn an_accepted_credential_says_so_once_and_starts_again_from_nothing() {
        let credential = Credential::new();

        credential.refused_at(Some(1), at(0));
        credential.refused_at(Some(1), at(5));

        assert_eq!(
            credential.accepted_at(at(20)),
            Accepted::Again {
                refused_for: Duration::from_secs(20)
            },
        );
        assert!(credential.due_at(at(20)), "and the wait is forgotten");
        assert_eq!(credential.accepted_at(at(30)), Accepted::Quiet);
        assert_eq!(
            credential.refused_at(Some(1), at(40)),
            Refused::First {
                retry_in: RETRY_MIN
            },
            "a second run of refusals is announced again",
        );
    }

    #[test]
    fn a_credential_that_has_been_replaced_does_not_serve_the_old_one_s_sentence() {
        // The renewal that lands mid-backoff: Nomad rewrote the file, so what
        // we are holding is not the token the server refused.
        let credential = Credential::new();

        credential.refused_at(Some(fingerprint(&Secret::new("old"))), at(0));
        credential.refused_at(Some(fingerprint(&Secret::new("old"))), at(5));

        assert!(!credential.due_at(at(6)), "ten seconds of backoff");
        assert!(
            !credential.changed(fingerprint(&Secret::new("old"))),
            "the same token is the same token",
        );
        assert!(!credential.due_at(at(6)));

        assert!(credential.changed(fingerprint(&Secret::new("new"))));
        assert!(
            credential.due_at(at(6)),
            "a new credential is tried at once"
        );
        assert_eq!(
            credential.refused_at(Some(2), at(6)),
            Refused::Quiet,
            "and if it is refused too, the run of refusals is still the same run",
        );
        assert_eq!(
            credential.retry_in_at(at(6)),
            RETRY_MIN,
            "but its wait starts again from the shortest",
        );
    }

    #[test]
    fn a_credential_nobody_has_refused_yet_is_never_a_change() {
        let credential = Credential::new();

        assert!(!credential.changed(fingerprint(&Secret::new("first"))));
        assert!(credential.due_at(at(0)));
    }

    #[test]
    fn the_first_line_points_at_whichever_end_actually_refused() {
        // "The server answered, so the link is up" is a true sentence about a
        // 400 and a false one about a token file that is not there — and the
        // second sends an operator to the wrong end of the problem.
        assert!(whose_fault(true).contains("the control link is up"));
        assert!(whose_fault(false).contains("no credential to present"));
        assert!(!whose_fault(false).contains("server answered"));
    }

    #[test]
    fn a_fingerprint_is_a_number_and_never_the_credential() {
        let token = Secret::new("eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJhaXMifQ.signature");
        let mark = fingerprint(&token);

        assert_eq!(mark, fingerprint(&token), "the same token, the same mark");
        assert_ne!(mark, fingerprint(&Secret::new("something-else")));
        assert!(
            !format!("{mark}").contains("eyJ"),
            "a fingerprint carries none of the token",
        );
    }
}
