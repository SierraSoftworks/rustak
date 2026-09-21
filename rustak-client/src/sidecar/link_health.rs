//! Whether the control link is up, and what an operator should be told about
//! it.
//!
//! # Why a state machine and not a log line
//!
//! The first live deployment produced **82 warnings in two and a half minutes**
//! from one outage. Every control-API call — the token exchange, the event
//! feed, registration, the heartbeat, a plugin's configuration read — logged a
//! multi-line warning with its advice block, on every tick, for as long as the
//! link was down. The cause was in the first of those lines and the other 81
//! buried it.
//!
//! So failure is a *state*, not an event. A run of failures is announced once,
//! with the whole cause chain and its advice; while it lasts, the attempts that
//! keep failing are `debug`; every [`REMIND_EVERY`] a single line says how long
//! it has been failing and what the last error was; and the recovery is one
//! `info` line naming the duration. That is four lines for an outage of any
//! length, plus one every five minutes.
//!
//! # A refusal is not an outage
//!
//! [`Failure`] is the distinction that keeps this honest. A `409` or a `404` is
//! the server *answering*: the link is up, however unwelcome the answer, so it
//! is throttled like any other repetition but it does not stop anything else
//! being tried. Only a failure to reach the server at all —
//! [`http::is_transport`](crate::http::is_transport) — marks the link down.
//!
//! Getting that wrong is worse than the noise it fixes: an events route that
//! answers `404` would otherwise leave a perfectly healthy sidecar silently not
//! heartbeating.
//!
//! # And being down means it can be backed off
//!
//! A link that is known to be down is not worth calling every tick. Each
//! unreachable attempt doubles the wait, from [`RETRY_MIN`] to [`RETRY_MAX`],
//! and anything that reaches the server resets it — so an outage costs a
//! handful of attempts rather than one per tick, and the heartbeats in between
//! are skipped rather than sent into the dark and logged.
//!
//! Nothing here is plugin-visible: a sidecar's job is the CoT it publishes, and
//! a control API that is down has never been a reason to stop.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// How long after a failure the next attempt is worth making.
pub(crate) const RETRY_MIN: Duration = Duration::from_secs(1);

/// The longest that wait becomes, however many times it has failed.
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(60);

/// How often a run of failures that is still going is mentioned again.
const REMIND_EVERY: Duration = Duration::from_secs(300);

/// What a failed call says about the link as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The server was never reached: a handshake, a DNS lookup, a connection,
    /// a timeout. The link is down, and other calls are not worth making.
    Unreachable,

    /// The server answered and the answer was a refusal. The link is **up**;
    /// only this call failed.
    Refused,
}

/// What one attempt's outcome is worth saying out loud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Report {
    /// Nothing new: log the attempt at `debug` and leave it there.
    Quiet,

    /// The first failure of a run — one warning, with the cause chain and the
    /// advice that goes with it.
    First,

    /// Still failing. One line: how long, and the last error.
    Reminder { failing_for: Duration },

    /// It is working again. One line: how long it was not.
    Recovered { failing_for: Duration },
}

/// The pure half: an outage as it looks over time.
///
/// Every method takes the instant to judge against, so the tests move the clock
/// by hand and nothing here waits.
#[derive(Debug, Default)]
struct State {
    /// When the current run of failures started; [`None`] while calls work.
    failing_since: Option<DateTime<Utc>>,

    /// When this run was last mentioned at `warn`.
    mentioned_at: Option<DateTime<Utc>>,

    /// Whether the server is currently out of reach, which is what gates
    /// attempts. A refusal clears it: the server answered.
    unreachable: bool,

    /// How many unreachable attempts in a row, for the backoff.
    failures: u32,

    /// The earliest instant another attempt is worth making.
    retry_at: Option<DateTime<Utc>>,
}

impl State {
    /// Whether an attempt is worth making now.
    fn due(&self, now: DateTime<Utc>) -> bool {
        match self.retry_at {
            Some(at) => now >= at,
            None => true,
        }
    }

    /// Records a failure and answers what to say about it.
    fn failed(&mut self, failure: Failure, now: DateTime<Utc>) -> Report {
        match failure {
            Failure::Unreachable => {
                self.unreachable = true;
                self.failures = self.failures.saturating_add(1);
                self.retry_at = Some(now + backoff(self.failures));
            }
            // The server answered, so there is nothing to back off from and
            // nothing to stop the next call reaching it.
            Failure::Refused => self.reached(),
        }

        self.mention(now)
    }

    /// Records a success and answers what to say about it.
    fn succeeded(&mut self, now: DateTime<Utc>) -> Report {
        self.reached();
        self.mentioned_at = None;

        match self.failing_since.take() {
            Some(since) => Report::Recovered {
                failing_for: elapsed(since, now),
            },
            None => Report::Quiet,
        }
    }

    /// Everything that "the server answered" means, whatever it answered.
    fn reached(&mut self) {
        self.unreachable = false;
        self.failures = 0;
        self.retry_at = None;
    }

    /// Whether this failure is worth a `warn`, and what it should say.
    fn mention(&mut self, now: DateTime<Utc>) -> Report {
        let Some(since) = self.failing_since else {
            self.failing_since = Some(now);
            self.mentioned_at = Some(now);

            return Report::First;
        };

        let quiet_for = self
            .mentioned_at
            .map_or(REMIND_EVERY, |at| elapsed(at, now));

        if quiet_for < REMIND_EVERY {
            return Report::Quiet;
        }

        self.mentioned_at = Some(now);

        Report::Reminder {
            failing_for: elapsed(since, now),
        }
    }
}

/// How long to wait after `failures` consecutive failures.
///
/// Capped exponential: 1s, 2s, 4s … 60s. `saturating_sub` and the cap keep the
/// shift away from overflowing however long an outage runs.
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);

    RETRY_MIN.saturating_mul(1u32 << doublings).min(RETRY_MAX)
}

/// `now - before`, never negative — a clock that stepped backwards is not a
/// negative outage.
fn elapsed(before: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - before).to_std().unwrap_or_default()
}

/// The control link's health, shared between the harness loop and the feed
/// task.
///
/// One link, one notion of "down": a feed that reconnected and a heartbeat that
/// landed are the same recovery, and an operator should be told about it once.
#[derive(Debug, Default)]
pub(crate) struct LinkHealth {
    state: Mutex<State>,
}

impl LinkHealth {
    /// A link nothing has been said about yet, which is assumed up.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether an attempt is worth making now.
    ///
    /// `true` whenever the link is up; while it is down, only once the backoff
    /// has run out.
    pub(crate) fn due(&self) -> bool {
        self.due_at(Utc::now())
    }

    /// [`due`](Self::due), at an instant of the caller's choosing.
    pub(crate) fn due_at(&self, now: DateTime<Utc>) -> bool {
        self.with(|state| state.due(now))
    }

    /// Whether the server is currently believed to be out of reach.
    pub(crate) fn is_down(&self) -> bool {
        self.with(|state| state.unreachable)
    }

    /// How long to wait before the next attempt, for a caller that sleeps.
    pub(crate) fn backoff(&self) -> Duration {
        self.with(|state| backoff(state.failures))
    }

    /// Records a failure and answers what to say about it.
    pub(crate) fn failed(&self, failure: Failure) -> Report {
        self.failed_at(failure, Utc::now())
    }

    /// [`failed`](Self::failed), at an instant of the caller's choosing.
    pub(crate) fn failed_at(&self, failure: Failure, now: DateTime<Utc>) -> Report {
        self.with(|state| state.failed(failure, now))
    }

    /// Records a success and answers what to say about it.
    pub(crate) fn succeeded(&self) -> Report {
        self.succeeded_at(Utc::now())
    }

    /// [`succeeded`](Self::succeeded), at an instant of the caller's choosing.
    pub(crate) fn succeeded_at(&self, now: DateTime<Utc>) -> Report {
        self.with(|state| state.succeeded(now))
    }

    /// Runs `act` against the state, taking a poisoned lock's contents rather
    /// than panicking: a link's health is not worth failing a sidecar over.
    fn with<T>(&self, act: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        act(&mut state)
    }
}

/// A duration as an operator reads it: "4m12s", "3s", "1h02m".
pub(crate) fn humanised(duration: Duration) -> String {
    let seconds = duration.as_secs();

    match (seconds / 3600, (seconds % 3600) / 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m{seconds:02}s"),
        (hours, minutes, _) => format!("{hours}h{minutes:02}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    /// The failure an outage is made of.
    fn gone(health: &LinkHealth, seconds: i64) -> Report {
        health.failed_at(Failure::Unreachable, at(seconds))
    }

    #[test]
    fn the_first_failure_of_an_outage_is_the_one_that_is_announced() {
        let health = LinkHealth::new();

        assert_eq!(gone(&health, 0), Report::First);
        assert!(health.is_down());
    }

    #[test]
    fn every_failure_after_it_is_quiet_until_the_reminder_falls_due() {
        // The production finding: 82 warnings in two and a half minutes, 81 of
        // them repeating the first.
        let health = LinkHealth::new();

        assert_eq!(gone(&health, 0), Report::First);

        for second in [1, 3, 7, 15, 31, 91, 151, 240, 299] {
            assert_eq!(
                gone(&health, second),
                Report::Quiet,
                "the outage was already announced at 0s; {second}s must be quiet",
            );
        }
    }

    #[test]
    fn an_outage_that_is_still_going_is_mentioned_once_every_five_minutes() {
        let health = LinkHealth::new();

        gone(&health, 0);

        assert_eq!(
            gone(&health, 300),
            Report::Reminder {
                failing_for: Duration::from_secs(300)
            },
        );
        assert_eq!(gone(&health, 400), Report::Quiet);
        assert_eq!(
            gone(&health, 600),
            Report::Reminder {
                failing_for: Duration::from_secs(600)
            },
            "the reminder is spaced from the last reminder, and measured from the first failure",
        );
    }

    #[test]
    fn coming_back_is_one_line_naming_how_long_it_was_gone() {
        let health = LinkHealth::new();

        gone(&health, 0);
        gone(&health, 30);

        assert_eq!(
            health.succeeded_at(at(90)),
            Report::Recovered {
                failing_for: Duration::from_secs(90)
            },
        );
        assert!(!health.is_down());
    }

    #[test]
    fn a_link_that_never_failed_says_nothing_when_a_call_succeeds() {
        // Otherwise every heartbeat of every healthy sidecar is a log line.
        let health = LinkHealth::new();

        assert_eq!(health.succeeded_at(at(0)), Report::Quiet);
        assert_eq!(health.succeeded_at(at(30)), Report::Quiet);
    }

    #[test]
    fn a_second_outage_is_announced_again_rather_than_folded_into_the_first() {
        let health = LinkHealth::new();

        gone(&health, 0);
        health.succeeded_at(at(10));

        assert_eq!(gone(&health, 20), Report::First);
    }

    #[test]
    fn a_refusal_is_throttled_but_leaves_the_link_up() {
        // The server answered. Repeating the same refusal every tick is still
        // noise, but it must not stop anything else being tried — an events
        // route answering 404 has nothing to do with the heartbeat route.
        let health = LinkHealth::new();

        assert_eq!(health.failed_at(Failure::Refused, at(0)), Report::First);
        assert!(!health.is_down(), "a refusal is not an outage");
        assert!(
            health.due_at(at(0)),
            "and nothing is held back because of it"
        );

        assert_eq!(health.failed_at(Failure::Refused, at(30)), Report::Quiet);
        assert_eq!(health.backoff(), RETRY_MIN, "there is nothing to back off");
    }

    #[test]
    fn a_server_that_answers_again_ends_the_outage_even_by_refusing() {
        // Reaching the server is what "the link is back" means; what it said is
        // the caller's problem.
        let health = LinkHealth::new();

        gone(&health, 0);
        gone(&health, 2);

        assert!(health.is_down());

        health.failed_at(Failure::Refused, at(10));

        assert!(!health.is_down());
        assert!(health.due_at(at(10)));
    }

    #[test]
    fn the_wait_doubles_to_a_minute_and_stops_there() {
        let health = LinkHealth::new();

        for (attempt, expected) in [(1, 1), (2, 2), (3, 4), (4, 8), (5, 16), (6, 32), (7, 60)] {
            gone(&health, attempt);

            assert_eq!(
                health.backoff(),
                Duration::from_secs(expected),
                "after {attempt} failures",
            );
        }

        // However long it runs, and without overflowing the shift.
        for attempt in 8..80 {
            gone(&health, attempt);
        }

        assert_eq!(health.backoff(), RETRY_MAX);
    }

    #[test]
    fn a_success_resets_the_wait() {
        let health = LinkHealth::new();

        gone(&health, 0);
        gone(&health, 1);
        gone(&health, 3);
        health.succeeded_at(at(7));

        assert_eq!(health.backoff(), RETRY_MIN);
        assert!(health.due_at(at(7)), "and the next attempt is due at once");
    }

    #[test]
    fn nothing_is_attempted_again_until_the_wait_has_run_out() {
        // The other half of the noise: a heartbeat every tick against a server
        // that is not answering is a request per tick as well as a line per
        // tick.
        let health = LinkHealth::new();

        assert!(health.due_at(at(0)), "a link that is up is always due");

        gone(&health, 0);

        assert!(!health.due_at(at(0)), "one second of backoff");
        assert!(health.due_at(at(1)));

        gone(&health, 1);

        assert!(!health.due_at(at(2)), "two seconds now");
        assert!(health.due_at(at(3)));
    }

    #[test]
    fn a_clock_that_stepped_backwards_is_not_a_negative_outage() {
        let health = LinkHealth::new();

        gone(&health, 100);

        assert_eq!(
            health.succeeded_at(at(40)),
            Report::Recovered {
                failing_for: Duration::ZERO
            },
        );
    }

    #[test]
    fn a_duration_reads_the_way_an_operator_would_say_it() {
        assert_eq!(humanised(Duration::from_secs(3)), "3s");
        assert_eq!(humanised(Duration::from_secs(59)), "59s");
        assert_eq!(humanised(Duration::from_secs(252)), "4m12s");
        assert_eq!(humanised(Duration::from_secs(3720)), "1h02m");
    }
}
