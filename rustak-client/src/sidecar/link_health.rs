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

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

/// How long after a failure the next attempt is worth making.
pub(crate) const RETRY_MIN: Duration = Duration::from_secs(1);

/// The longest that wait becomes, however many times it has failed.
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(60);

/// How often a run of failures that is still going is mentioned again.
pub(crate) const REMIND_EVERY: Duration = Duration::from_secs(300);

/// How many clean closes of the server-event feed within [`REMIND_EVERY`] stop
/// being ordinary and start being something an operator should look at.
///
/// A feed that a proxy, a load balancer or an idle timer is cutting reopens
/// cleanly every time, so nothing here fails and nothing would ever be said —
/// which is exactly the shape of the bug this milestone fixed.
const CHURN_CLOSES: usize = 5;

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
pub(crate) fn elapsed(before: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
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

/// What a *successful* opening of the server-event feed is worth saying.
///
/// The feed is opened over and over in the ordinary course of things — a
/// server restart, a token that expired under it, a proxy that recycled the
/// connection — and every one of those used to be an `info` line. So the same
/// rule as everything else here: announce a change, stay quiet about a
/// repetition, and count the repetitions into one reminder when there are
/// enough of them to be a fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opened {
    /// The first time, or the first time after an outage [`LinkHealth`]
    /// announced. One `info` line.
    Announce,

    /// A clean close and a clean reopen. `debug`, and nothing else.
    Quiet,

    /// Too many clean closes in too short a time. One counted `warn`.
    Churning { closes: usize, within: Duration },
}

/// How often the server-event feed has closed cleanly and been reopened.
///
/// Owned by the feed task rather than shared, because it is the feed task's
/// own history: [`LinkHealth`] is the *link*, which a heartbeat moves as well.
/// Every method takes the instant to judge against, so the tests move the clock
/// by hand and nothing here waits.
#[derive(Debug, Default)]
pub(crate) struct Reopenings {
    /// Whether a feed has ever been open, so the first one is announced.
    opened_once: bool,

    /// When each recent clean close happened, oldest first, pruned to
    /// [`REMIND_EVERY`].
    closes: VecDeque<DateTime<Utc>>,

    /// When the churn was last mentioned at `warn`.
    warned_at: Option<DateTime<Utc>>,
}

impl Reopenings {
    /// A feed nothing has been said about yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records a clean close — the server ended the response, or it went idle.
    pub(crate) fn closed(&mut self, now: DateTime<Utc>) {
        self.closes.push_back(now);
        self.prune(now);
    }

    /// Records a successful opening and answers what to say about it.
    ///
    /// `recovered` is whether [`LinkHealth`] has just reported the link back
    /// from an outage: that is a change worth announcing, and the reopening is
    /// the line that says the feed came back with it.
    pub(crate) fn opened(&mut self, recovered: bool, now: DateTime<Utc>) -> Opened {
        self.prune(now);

        if !self.opened_once {
            self.opened_once = true;
            // A first open is not a reopen, whatever happened while trying.
            self.closes.clear();

            return Opened::Announce;
        }

        if recovered {
            return Opened::Announce;
        }

        let quiet_for = self.warned_at.map_or(REMIND_EVERY, |at| elapsed(at, now));

        if self.closes.len() > CHURN_CLOSES && quiet_for >= REMIND_EVERY {
            self.warned_at = Some(now);

            return Opened::Churning {
                closes: self.closes.len(),
                within: REMIND_EVERY,
            };
        }

        Opened::Quiet
    }

    /// Forgets the closes that are older than the window they are counted over.
    fn prune(&mut self, now: DateTime<Utc>) {
        while self
            .closes
            .front()
            .is_some_and(|at| elapsed(*at, now) > REMIND_EVERY)
        {
            self.closes.pop_front();
        }
    }
}

/// Records a failed control-API call and says as much about it as its state
/// calls for.
///
/// Classified by [`http::is_transport`](crate::http::is_transport): only a
/// server we could not reach is an outage, and a refusal — which is the server
/// answering — leaves everything else free to carry on.
pub(crate) fn note(health: &LinkHealth, what: &str, err: &Error) {
    let failure = match crate::http::is_transport(err) {
        true => Failure::Unreachable,
        false => Failure::Refused,
    };

    announce(health.failed(failure), failure, what, err);
}

/// Logs one control-API failure at the level the link's state calls for.
///
/// The first failure of a run carries the whole rendered error — the cause
/// chain `http::transport` built, and the advice that names
/// `[service] control_truststore` — because that is the line an operator reads.
/// Everything after it is `debug` until the state changes, and a reminder is
/// one line rather than a block.
pub(crate) fn announce(report: Report, failure: Failure, what: &str, err: &Error) {
    match (report, failure) {
        (Report::First, Failure::Unreachable) => tracing::warn!(
            error = %err,
            "Could not {what}. The control link is down; further failures are logged at debug until it is back.",
        ),
        (Report::First, Failure::Refused) => tracing::warn!(
            error = %err,
            "Could not {what}. The server answered, so the link is up; repeats are logged at debug until this changes.",
        ),
        (Report::Reminder { failing_for }, _) => tracing::warn!(
            error = %err.description(),
            "The control link has been failing for {}; the last attempt was to {what}.",
            humanised(failing_for),
        ),
        (Report::Quiet, _) => tracing::debug!(
            error = %err.description(),
            "Could not {what}; nothing has changed since this was last reported.",
        ),
        // A failure cannot be a recovery.
        (Report::Recovered { .. }, _) => {}
    }
}

/// Says so, once, when the link starts working again.
pub(crate) fn recovered(report: Report) {
    if let Report::Recovered { failing_for } = report {
        tracing::info!("The control link is back after {}.", humanised(failing_for));
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
    fn the_first_opening_of_the_feed_is_the_one_that_is_announced() {
        let mut feed = Reopenings::new();

        assert_eq!(feed.opened(false, at(0)), Opened::Announce);
    }

    #[test]
    fn a_clean_close_and_a_clean_reopen_say_nothing_at_info() {
        // The production finding: a total timeout cut the feed every 31s and
        // every reopening was an `info` line an operator had to read as a
        // fault. A close that is followed by a successful open is not news.
        let mut feed = Reopenings::new();

        feed.opened(false, at(0));

        for second in [31, 62, 93] {
            feed.closed(at(second - 1));

            assert_eq!(
                feed.opened(false, at(second)),
                Opened::Quiet,
                "the reopening at {second}s must be quiet",
            );
        }
    }

    #[test]
    fn a_feed_that_keeps_being_cut_is_mentioned_once_with_a_count() {
        // Quiet is not the same as silent: something that closes a feed six
        // times in five minutes is a proxy or an idle timer, and an operator
        // should be told once rather than never.
        let mut feed = Reopenings::new();

        feed.opened(false, at(0));

        for nth in 1..=5 {
            feed.closed(at(nth * 10));
            assert_eq!(feed.opened(false, at(nth * 10 + 1)), Opened::Quiet);
        }

        feed.closed(at(60));

        assert_eq!(
            feed.opened(false, at(61)),
            Opened::Churning {
                closes: 6,
                within: REMIND_EVERY,
            },
        );

        // And then it is quiet again until the next window.
        feed.closed(at(70));
        assert_eq!(feed.opened(false, at(71)), Opened::Quiet);
    }

    #[test]
    fn closes_older_than_the_window_are_not_counted_towards_the_churn() {
        // A feed reopened once an hour for six hours is a healthy feed.
        let mut feed = Reopenings::new();

        feed.opened(false, at(0));

        for nth in 1..=8 {
            feed.closed(at(nth * 3_600));
            assert_eq!(feed.opened(false, at(nth * 3_600 + 1)), Opened::Quiet);
        }
    }

    #[test]
    fn a_feed_that_comes_back_from_an_outage_says_so_even_though_it_reopened() {
        // The one reopening that is always worth a line: the link was down,
        // `LinkHealth` said so, and this is the other half of that sentence.
        let mut feed = Reopenings::new();

        feed.opened(false, at(0));
        feed.closed(at(10));

        assert_eq!(feed.opened(true, at(40)), Opened::Announce);
    }

    #[test]
    fn a_failure_before_the_first_open_does_not_make_the_first_open_a_reopen() {
        // A sidecar that starts before the server does fails, retries and then
        // opens: that first success is a first connection, not a reconnection.
        let mut feed = Reopenings::new();

        feed.closed(at(0));
        feed.closed(at(1));

        assert_eq!(feed.opened(false, at(2)), Opened::Announce);

        feed.closed(at(3));
        assert_eq!(feed.opened(false, at(4)), Opened::Quiet);
    }

    #[test]
    fn a_duration_reads_the_way_an_operator_would_say_it() {
        assert_eq!(humanised(Duration::from_secs(3)), "3s");
        assert_eq!(humanised(Duration::from_secs(59)), "59s");
        assert_eq!(humanised(Duration::from_secs(252)), "4m12s");
        assert_eq!(humanised(Duration::from_secs(3720)), "1h02m");
    }
}
