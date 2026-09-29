//! What an upstream is doing, and when it is worth asking again.
//!
//! A floor under the request rate that is independent of the sidecar's tick, a
//! capped backoff, and enough memory to tell an administrator whether the feed
//! is fine, struggling, or has never worked.
//!
//! It logs **changes** of state and nothing else. Each of the two things that
//! go on for a while — the upstream not answering, and the upstream saying
//! "not so fast" — is a [`Repeated`] run: one line when it starts, one every
//! [`REMIND_EVERY`] while it lasts, one when it is over, and `debug` for every
//! attempt in between, so an outage of any length is a handful of lines.
//!
//! # `poll` is a floor, not a pin
//!
//! The interval is the fastest this source asks. A `Retry-After` that ESB
//! states is honoured above it, up to [`MAX_RETRY_AFTER`]; one it does not
//! state is waited out on twice the interval, and said to be our own guess.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::notice::{REMIND_EVERY, Repeated, Report, humanised};

/// The longest a source waits between attempts, however many have failed.
pub const MAX_BACKOFF: Duration = Duration::from_secs(900);

/// The longest a stated `Retry-After` is believed. An upstream is taken at its
/// word well past [`MAX_BACKOFF`], but a header that says "next week" is a
/// misconfiguration somewhere, and not a reason to go dark through a storm.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Doublings allowed before [`MAX_BACKOFF`] catches the backoff anyway.
const MAX_DOUBLINGS: u32 = 6;

/// How an upstream is doing.
#[derive(Clone, Debug)]
pub struct SourceState {
    name: String,
    interval: Duration,
    connected: bool,
    failures: u32,
    since: DateTime<Utc>,
    last_success: Option<DateTime<Utc>>,
    last_error: Option<String>,
    next_attempt: DateTime<Utc>,

    /// The run of failures, so an outage is announced once.
    outage: Repeated,

    /// The run of `429`s. It settles rather than ending on the next answer,
    /// so a provider refusing every other request is one run and not many.
    limit: Repeated,
}

impl SourceState {
    /// A source that has not been asked anything yet, and may be asked now.
    #[must_use]
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self::new_at(name, interval, Utc::now())
    }

    /// [`new`](Self::new), at an instant of the caller's choosing.
    #[must_use]
    pub fn new_at(name: impl Into<String>, interval: Duration, now: DateTime<Utc>) -> Self {
        Self {
            name: name.into(),
            interval,
            connected: false,
            failures: 0,
            since: now,
            last_success: None,
            last_error: None,
            next_attempt: now,
            outage: Repeated::new(Duration::ZERO),
            limit: Repeated::new(REMIND_EVERY.max(interval.saturating_mul(3))),
        }
    }

    /// The upstream's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often the upstream is asked, at the fastest.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether it is time to reach out again.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.ready_at(Utc::now())
    }

    /// [`ready`](Self::ready), at an instant of the caller's choosing.
    #[must_use]
    pub fn ready_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.next_attempt
    }

    /// Makes the next request due now, keeping the interval and the backoff
    /// that follows from it, so a test never waits five minutes for a poll.
    #[cfg(test)]
    pub(crate) fn due_now(&mut self) {
        self.next_attempt = Utc::now();
    }

    /// Moves the last answer into the past, so a test can stand two hours
    /// into an upstream outage without waiting for one.
    #[cfg(test)]
    pub(crate) fn answered_at(&mut self, at: DateTime<Utc>) {
        self.last_success = Some(at);
    }

    /// Records an answer, and schedules the next request an interval away.
    pub fn succeeded(&mut self) {
        self.succeeded_at(Utc::now());
    }

    /// [`succeeded`](Self::succeeded), at an instant of the caller's choosing.
    /// Answers what was said about the outage run and the rate-limit run, in
    /// that order, for the tests.
    pub fn succeeded_at(&mut self, now: DateTime<Utc>) -> (Report, Report) {
        let report = self.outage.cleared(now);

        match report {
            Report::Recovered { count, over } => {
                info!(source = %self.name, "{} answered again after {} and {count} failed attempts.", self.name, humanised(over));
                self.since = now;
            }
            _ if self.last_success.is_none() => {
                info!(source = %self.name, "{} is answering.", self.name);
                self.since = now;
            }
            _ => {}
        }

        let limit = self.limit.cleared(now);
        if let Report::Recovered { count, over } = limit {
            info!(source = %self.name, "{} has stopped refusing requests; it refused {count} over {}.", self.name, humanised(over));
        }

        self.connected = true;
        self.failures = 0;
        self.last_success = Some(now);
        self.last_error = None;
        self.next_attempt = now + self.interval;

        (report, limit)
    }

    /// Records a failure, and backs off. `error` must be fit for an
    /// administrator to read; nothing here should ever be handed a credential.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.failed_at(error, Utc::now());
    }

    /// [`failed`](Self::failed), at an instant of the caller's choosing.
    /// Answers what was said about it, for the tests.
    pub fn failed_at(&mut self, error: impl Into<String>, now: DateTime<Utc>) -> Report {
        let error = error.into();
        let report = self.outage.happened(now);

        match report {
            Report::First => {
                self.since = now;
                warn!(source = %self.name, "{} stopped answering; retrying with backoff. {error}", self.name);
            }
            Report::Reminder { count, over } => warn!(
                source = %self.name,
                "{} has not answered for {}; {count} attempts failed in the last {}. {error}",
                self.name,
                humanised(elapsed(self.since, now)),
                humanised(over),
            ),
            _ => debug!(source = %self.name, "{} is still not answering. {error}", self.name),
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();

        report
    }

    /// Holds off because the upstream said "not so fast", which is an upstream
    /// that is working: the connection state is left alone.
    ///
    /// A stated delay is honoured up to [`MAX_RETRY_AFTER`], and above the
    /// interval however short the interval is; without one the guess is twice
    /// the interval. Answers how long the wait is, so a source can hold its
    /// other requests back for as long.
    pub fn wait_for(&mut self, asked: Option<Duration>) -> Duration {
        self.wait_for_at(asked, Utc::now()).0
    }

    /// [`wait_for`](Self::wait_for), at an instant of the caller's choosing.
    /// Answers the wait and what was said about it.
    pub fn wait_for_at(
        &mut self,
        asked: Option<Duration>,
        now: DateTime<Utc>,
    ) -> (Duration, Report) {
        let delay = asked
            .map_or((self.interval * 2).min(MAX_BACKOFF), |stated| {
                stated.min(MAX_RETRY_AFTER)
            })
            .max(self.interval);
        let (seconds, stated) = (delay.as_secs(), asked.is_some());
        let report = self.limit.happened(now);

        // Said as what happened: a wait ESB named is ESB's, and one it did not
        // name is our own guess and must not be attributed to it.
        match (report, stated) {
            (Report::First, true) => {
                info!(source = %self.name, seconds, stated, "{} asked us to wait {seconds}s.", self.name)
            }
            (Report::First, false) => {
                info!(source = %self.name, seconds, stated, "{} refused a request (429) without naming a delay; waiting {seconds}s.", self.name)
            }
            (Report::Reminder { count, over }, _) => {
                info!(source = %self.name, seconds, stated, "{} is still refusing requests: {count} in the last {}; waiting {seconds}s.", self.name, humanised(over))
            }
            _ => {
                debug!(source = %self.name, seconds, stated, "{} refused a request again; waiting {seconds}s.", self.name)
            }
        }

        self.next_attempt = now + delay;

        (delay, report)
    }

    /// Whether the last attempt worked.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// When the upstream last answered, if it ever has.
    #[must_use]
    pub const fn last_success(&self) -> Option<DateTime<Utc>> {
        self.last_success
    }

    /// When the current condition began.
    #[must_use]
    pub const fn since(&self) -> DateTime<Utc> {
        self.since
    }

    /// What went wrong last, if anything has.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The interval, doubled per consecutive failure, capped at [`MAX_BACKOFF`].
    fn backoff(&self) -> Duration {
        let doublings = self.failures.saturating_sub(1).min(MAX_DOUBLINGS);

        self.interval
            .saturating_mul(1_u32 << doublings)
            .min(MAX_BACKOFF)
            .max(self.interval)
    }
}

/// `now - before`, never negative.
fn elapsed(before: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - before).to_std().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    fn state() -> SourceState {
        SourceState::new_at("PowerCheck", Duration::from_secs(300), at(0))
    }

    #[test]
    fn a_stated_delay_is_believed_only_so_far() {
        let week = Duration::from_secs(7 * 24 * 3600);

        assert_eq!(state().wait_for_at(Some(week), at(0)).0, MAX_RETRY_AFTER);
    }

    #[test]
    fn a_new_source_may_be_asked_at_once_and_has_never_answered() {
        let state = state();

        assert!(state.ready_at(at(0)));
        assert!(!state.is_connected());
        assert_eq!(state.last_success(), None);
    }

    #[test]
    fn an_answer_holds_the_next_request_off_for_an_interval() {
        let mut state = state();

        state.succeeded_at(at(0));

        assert!(state.is_connected());
        assert!(!state.ready_at(at(299)));
        assert!(state.ready_at(at(300)));
        assert!(state.last_success().is_some());
    }

    #[test]
    fn a_failure_is_remembered_and_backed_off_from() {
        let mut state = state();
        state.succeeded_at(at(0));

        state.failed_at("connection refused", at(300));

        assert!(!state.is_connected());
        assert_eq!(state.last_error(), Some("connection refused"));
        assert!(state.last_success().is_some(), "it did work once");
        assert!(!state.ready_at(at(599)));
    }

    #[test]
    fn being_asked_to_wait_is_not_a_failure() {
        let mut state = state();
        state.succeeded_at(at(0));

        let (wait, _) = state.wait_for_at(Some(Duration::from_secs(600)), at(300));

        assert_eq!(wait, Duration::from_secs(600), "longer than any backoff");
        assert!(state.is_connected());
        assert_eq!(state.last_error(), None);
        assert!(!state.ready_at(at(899)));
    }

    #[test]
    fn an_explicit_poll_is_a_floor_a_stated_retry_after_raises() {
        // `poll = "1m"`, and ESB says ten minutes: ten minutes it is, and the
        // interval is back to the operator's one once ESB answers again.
        let mut state = SourceState::new_at("PowerCheck", Duration::from_secs(60), at(0));

        let (wait, _) = state.wait_for_at(Some(Duration::from_secs(600)), at(0));

        assert_eq!(wait, Duration::from_secs(600));
        assert!(!state.ready_at(at(599)), "not at the operator's minute");
        assert!(state.ready_at(at(600)));

        let (wait, _) = state.wait_for_at(Some(Duration::from_secs(5)), at(600));

        assert_eq!(wait, Duration::from_secs(60), "and never below it");

        state.succeeded_at(at(660));

        assert!(state.ready_at(at(720)), "the floor is where it was");
    }

    #[test]
    fn an_outage_is_said_once_when_it_starts_and_once_when_it_ends() {
        let mut state = state();
        state.succeeded_at(at(0));

        let said: Vec<Report> = (1..=4)
            .map(|nth| state.failed_at("timed out", at(nth * 60)))
            .collect();

        assert_eq!(said[0], Report::First);
        assert!(
            said[1..].iter().all(|report| *report == Report::Quiet),
            "{said:?}"
        );
        assert!(matches!(
            state.succeeded_at(at(290)).0,
            Report::Recovered { count: 4, .. }
        ));
        assert_eq!(
            state.succeeded_at(at(590)).0,
            Report::Quiet,
            "and only once"
        );
    }

    #[test]
    fn a_refusal_is_said_once_when_it_starts_and_once_when_it_is_over() {
        let mut state = state();
        state.succeeded_at(at(0));

        assert_eq!(state.wait_for_at(None, at(300)).1, Report::First);
        assert_eq!(state.wait_for_at(None, at(360)).1, Report::Quiet);

        // Answered, but the run has not settled: a provider refusing every
        // other request is one run, not one line per refusal.
        assert_eq!(state.succeeded_at(at(960)).1, Report::Quiet);
        assert!(
            matches!(
                state.wait_for_at(None, at(1260)).1,
                Report::Reminder { count: 2, .. }
            ),
            "a reminder, with a count, and not a new run",
        );

        assert!(matches!(
            state.succeeded_at(at(1260 + 900)).1,
            Report::Recovered { count: 3, .. }
        ));
        assert_eq!(state.succeeded_at(at(3000)).1, Report::Quiet, "once");
    }
}
