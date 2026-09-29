//! What the upstream is doing, and when it is worth asking again.
//!
//! The live source is an HTTP GET on a timer, so it wants three things: a floor
//! under how often it reaches out that is independent of the sidecar's tick, a
//! capped backoff when the answer is a failure, and enough memory of what
//! happened to tell an administrator whether the feed is fine, struggling or
//! has never worked at all.
//!
//! This is the only place in the crate that logs a **state change**. Each of
//! the two things that go on for a while — FIRMS not answering, and FIRMS
//! saying "not so fast" — is a [`Repeated`] run, the same as in the ADS-B and
//! AIS plugins: one line when it starts, one every [`REMIND_EVERY`] at most
//! while it lasts, one when it is over, and `debug` for every attempt between.
//!
//! # `poll` is a floor, not a pin
//!
//! The interval is the fastest FIRMS is asked. A `Retry-After` it states is
//! honoured above it, up to [`MAX_BACKOFF`]; a `429` that states none is waited
//! out on twice the interval, and said to be our own guess.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::notice::{REMIND_EVERY, Repeated, Report, humanised};

/// The longest the source waits between attempts, however many have failed.
///
/// An hour: satellites pass a few times a day, so a feed that has been down
/// all night loses nothing by waking up within the hour.
pub const MAX_BACKOFF: Duration = Duration::from_secs(3_600);

/// How many doublings the backoff is allowed before [`MAX_BACKOFF`] catches it
/// anyway, so the shift cannot overflow on a source that has failed for a week.
const MAX_DOUBLINGS: u32 = 6;

/// How an upstream is doing.
#[derive(Clone, Debug)]
pub struct SourceState {
    name: String,
    interval: Duration,
    connected: bool,
    ever_connected: bool,
    failures: u32,
    since: DateTime<Utc>,
    last_error: Option<String>,
    next_attempt: DateTime<Utc>,
    rate_limited: u64,

    /// The run of failures, so an outage is announced once.
    outage: Repeated,

    /// The run of `429`s. It settles over three polls rather than ending on
    /// the next answer, so a key refused every other poll is one run.
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
            ever_connected: false,
            failures: 0,
            since: now,
            last_error: None,
            next_attempt: now,
            rate_limited: 0,
            outage: Repeated::new(Duration::ZERO),
            limit: Repeated::new(REMIND_EVERY.max(interval.saturating_mul(3))),
        }
    }

    /// The upstream's name, for a log line or a heartbeat.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often this source reaches its upstream, at the fastest.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// How many times the upstream has said "not so fast".
    #[must_use]
    pub const fn rate_limited(&self) -> u64 {
        self.rate_limited
    }

    /// Whether it is time to reach out again. A source polls on the sidecar's
    /// tick, which is far more often than FIRMS has anything new to say.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.ready_at(Utc::now())
    }

    /// [`ready`](Self::ready), at an instant of the caller's choosing.
    #[must_use]
    pub fn ready_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.next_attempt
    }

    /// Records an answer, and schedules the next attempt one interval away.
    pub fn succeeded(&mut self) {
        self.succeeded_at(Utc::now());
    }

    /// [`succeeded`](Self::succeeded), at an instant of the caller's choosing.
    /// Answers what was said about the outage run and the rate-limit run, in
    /// that order, for the tests.
    pub fn succeeded_at(&mut self, now: DateTime<Utc>) -> (Report, Report) {
        let outage = self.answered(now);
        let limit = self.limit.cleared(now);

        if let Report::Recovered { count, over } = limit {
            info!(
                source = %self.name,
                "The FIRMS source has stopped refusing requests; it refused {count} over {}.",
                humanised(over),
            );
        }

        self.next_attempt = now + self.interval;

        (outage, limit)
    }

    /// Records a failure, and backs off.
    ///
    /// `error` must already be fit for an administrator to read, and free of
    /// the MAP_KEY: nothing here redacts.
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
                warn!(
                    source = %self.name,
                    "The FIRMS source stopped answering; retrying with backoff. {error}",
                );
            }
            Report::Reminder { count, over } => warn!(
                source = %self.name,
                "The FIRMS source has not answered for {}; {count} attempts failed in the last {}. {error}",
                humanised(elapsed(self.since, now)),
                humanised(over),
            ),
            _ => debug!(source = %self.name, "The FIRMS source is still not answering. {error}"),
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();

        report
    }

    /// Holds off because the upstream said "not so fast".
    ///
    /// Not a failure, and more than that an **answer**: an upstream that is rate
    /// limiting is reachable and working, so this counts as connected. A source
    /// whose very first reply is a `429` is therefore not reported as one that
    /// has never worked, which would send an administrator looking for a wrong
    /// setting that is not there.
    ///
    /// A stated delay is honoured above the interval and up to [`MAX_BACKOFF`]
    /// and no further, which is the ceiling the ADS-B plugin puts on
    /// `Retry-After` too: FIRMS' quota is a ten-minute window, so a longer wait
    /// is a mistake somewhere, and a mistake must not silence a fire feed for a
    /// day.
    pub fn wait_for(&mut self, asked: Option<Duration>) {
        self.wait_for_at(asked, Utc::now());
    }

    /// [`wait_for`](Self::wait_for), at an instant of the caller's choosing.
    /// Answers what was said about the rate-limit run, for the tests.
    pub fn wait_for_at(&mut self, asked: Option<Duration>, now: DateTime<Utc>) -> Report {
        let delay = asked
            .unwrap_or(self.interval * 2)
            .clamp(self.interval, MAX_BACKOFF.max(self.interval));
        let (seconds, stated) = (delay.as_secs(), asked.is_some());

        self.answered(now);
        self.rate_limited = self.rate_limited.saturating_add(1);
        self.next_attempt = now + delay;

        let report = self.limit.happened(now);

        // Said as what happened: a wait FIRMS named is FIRMS', and one it did
        // not name is our own guess and must not be attributed to it.
        match (report, stated) {
            (Report::First, true) => {
                info!(source = %self.name, seconds, stated, "The FIRMS source asked us to wait {seconds}s before the next request.")
            }
            (Report::First, false) => {
                info!(source = %self.name, seconds, stated, "The FIRMS source refused a request (429) without naming a delay; waiting {seconds}s.")
            }
            (Report::Reminder { count, over }, _) => {
                info!(source = %self.name, seconds, stated, "The FIRMS source is still refusing requests: {count} in the last {}; waiting {seconds}s.", humanised(over))
            }
            _ => {
                debug!(source = %self.name, seconds, stated, "The FIRMS source refused a request again; waiting {seconds}s.")
            }
        }

        report
    }

    /// Records that the upstream answered at all, which ends an outage.
    fn answered(&mut self, now: DateTime<Utc>) -> Report {
        let report = self.outage.cleared(now);

        match report {
            Report::Recovered { count, over } => info!(
                source = %self.name,
                "The FIRMS source answered again after {} and {count} failed attempts; the feed is connected.",
                humanised(over),
            ),
            _ if !self.ever_connected => info!(
                source = %self.name,
                "The FIRMS source answered; the feed is connected.",
            ),
            _ => {}
        }

        if !self.connected {
            self.since = now;
        }

        self.connected = true;
        self.ever_connected = true;
        self.failures = 0;
        self.last_error = None;

        report
    }

    /// Whether the last attempt worked.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Whether any attempt has ever worked: the difference between an outage
    /// to wait out and a configuration to look at.
    #[must_use]
    pub const fn ever_connected(&self) -> bool {
        self.ever_connected
    }

    /// When the current condition began.
    #[must_use]
    pub const fn since(&self) -> DateTime<Utc> {
        self.since
    }

    /// How long it has been failing, or [`None`] while it is working.
    #[must_use]
    pub fn reconnecting_for(&self) -> Option<chrono::Duration> {
        (!self.connected).then(|| Utc::now() - self.since)
    }

    /// What went wrong last, if anything has.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The wait before the next attempt: the interval, doubled once per
    /// consecutive failure, capped at [`MAX_BACKOFF`].
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

    const TEN_MINUTES: Duration = Duration::from_secs(600);

    fn now() -> DateTime<Utc> {
        "2026-09-22T14:00:00Z".parse().expect("an instant")
    }

    fn after(minutes: i64) -> DateTime<Utc> {
        now() + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn a_source_is_asked_once_an_interval_whatever_the_tick_is() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());

        assert!(state.ready_at(now()), "a new source may be asked at once");

        state.succeeded_at(now());

        assert!(!state.ready_at(after(9)));
        assert!(state.ready_at(after(10)));
        assert!(state.is_connected() && state.ever_connected());
    }

    #[test]
    fn failures_back_off_to_a_ceiling_and_a_success_resets_them() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        let mut waits = Vec::new();

        for _ in 0..5 {
            state.failed_at("timed out", now());
            waits.push((state.next_attempt - now()).num_minutes());
        }

        assert_eq!(waits, [10, 20, 40, 60, 60]);
        assert!(!state.ever_connected(), "it has never worked");
        assert_eq!(state.last_error(), Some("timed out"));

        state.succeeded_at(now());

        assert_eq!(state.last_error(), None);
        assert_eq!((state.next_attempt - now()).num_minutes(), 10);
    }

    #[test]
    fn a_rate_limit_is_an_answer_even_when_it_is_the_first_one() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        state.failed_at("timed out", now());

        state.wait_for_at(None, now());

        assert!(state.is_connected() && state.ever_connected());
        assert_eq!(state.last_error(), None);
    }

    #[test]
    fn being_asked_to_wait_is_not_an_outage() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        state.succeeded_at(now());

        state.wait_for_at(Some(Duration::from_secs(1_800)), now());

        assert!(state.is_connected());
        assert_eq!(state.rate_limited(), 1);
        assert!(!state.ready_at(after(29)));
        assert!(state.ready_at(after(30)));

        // A delay shorter than the interval never speeds the source up.
        state.wait_for_at(Some(Duration::from_secs(5)), now());

        assert!(!state.ready_at(after(9)));
    }

    #[test]
    fn an_explicit_poll_is_a_floor_a_stated_retry_after_raises() {
        // `poll = "1m"`, and FIRMS says fifteen minutes: fifteen it is, and the
        // operator's minute is the interval again once FIRMS answers.
        let mut state = SourceState::new_at("FIRMS", Duration::from_secs(60), now());

        state.wait_for_at(Some(Duration::from_secs(900)), now());

        assert!(!state.ready_at(after(14)), "not at the operator's minute");
        assert!(state.ready_at(after(15)));

        state.succeeded_at(after(15));

        assert!(!state.ready_at(after(15)));
        assert!(state.ready_at(after(16)), "the floor is where it was");
    }

    #[test]
    fn an_outage_is_said_once_when_it_starts_and_once_when_it_ends() {
        let mut state = SourceState::new_at("FIRMS", Duration::from_secs(60), now());
        state.succeeded_at(now());

        let said: Vec<Report> = (1..=4)
            .map(|minute| state.failed_at("timed out", after(minute)))
            .collect();

        assert_eq!(said[0], Report::First);
        assert!(
            said[1..].iter().all(|report| *report == Report::Quiet),
            "{said:?}"
        );
        assert!(matches!(
            state.succeeded_at(after(5)).0,
            Report::Recovered { count: 4, .. }
        ));
        assert_eq!(
            state.succeeded_at(after(6)).0,
            Report::Quiet,
            "and only once"
        );
    }

    #[test]
    fn a_refusal_is_said_once_when_it_starts_and_once_when_it_is_over() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        state.succeeded_at(now());

        assert_eq!(state.wait_for_at(None, after(10)), Report::First);

        // Answered twenty minutes later, as asked: the run has not settled,
        // so a key refused every other poll is one run, not a line a poll.
        assert_eq!(state.succeeded_at(after(30)).1, Report::Quiet);

        assert!(matches!(
            state.succeeded_at(after(40)).1,
            Report::Recovered { count: 1, .. }
        ));
        assert_eq!(state.succeeded_at(after(50)).1, Report::Quiet, "once");
    }
}
