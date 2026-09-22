//! What the upstream is doing, and when it is worth asking again.
//!
//! The live source is an HTTP GET on a timer, so it wants three things: a floor
//! under how often it reaches out that is independent of the sidecar's tick, a
//! capped backoff when the answer is a failure, and enough memory of what
//! happened to tell an administrator whether the feed is fine, struggling or
//! has never worked at all.
//!
//! This is the only place in the crate that logs a **state change**: the first
//! failure of a run at `warn`, the recovery at `info`, and everything between
//! at `debug`. FIRMS is polled every few minutes, so that is already quiet; the
//! reminder machinery the ADS-B and AIS plugins carry for second-by-second
//! feeds would be weight without a purpose here.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

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
        }
    }

    /// The upstream's name, for a log line or a heartbeat.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often this source reaches its upstream.
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
    pub fn succeeded_at(&mut self, now: DateTime<Utc>) {
        if !self.connected {
            info!(
                source = %self.name,
                failed_attempts = self.failures,
                "The FIRMS source answered; the feed is connected.",
            );
            self.since = now;
        }

        self.connected = true;
        self.ever_connected = true;
        self.failures = 0;
        self.last_error = None;
        self.next_attempt = now + self.interval;
    }

    /// Records a failure, and backs off.
    ///
    /// `error` must already be fit for an administrator to read, and free of
    /// the MAP_KEY: nothing here redacts.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.failed_at(error, Utc::now());
    }

    /// [`failed`](Self::failed), at an instant of the caller's choosing.
    pub fn failed_at(&mut self, error: impl Into<String>, now: DateTime<Utc>) {
        let error = error.into();

        if self.connected || self.failures == 0 {
            self.since = now;
            warn!(
                source = %self.name,
                "The FIRMS source stopped answering; retrying with backoff. {error}",
            );
        } else {
            debug!(source = %self.name, "The FIRMS source is still not answering. {error}");
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();
    }

    /// Holds off because the upstream said "not so fast".
    ///
    /// Not a failure, and more than that an **answer**: an upstream that is rate
    /// limiting is reachable and working, so this counts as connected. A source
    /// whose very first reply is a `429` is therefore not reported as one that
    /// has never worked, which would send an administrator looking for a wrong
    /// setting that is not there.
    ///
    /// A stated delay is honoured up to [`MAX_BACKOFF`] and no further, which is
    /// the ceiling the ADS-B plugin puts on `Retry-After` too: FIRMS' quota is a
    /// ten-minute window, so a longer wait is a mistake somewhere, and a mistake
    /// must not silence a fire feed for a day.
    pub fn wait_for(&mut self, asked: Option<Duration>) {
        self.wait_for_at(asked, Utc::now());
    }

    /// [`wait_for`](Self::wait_for), at an instant of the caller's choosing.
    pub fn wait_for_at(&mut self, asked: Option<Duration>, now: DateTime<Utc>) {
        let delay = asked
            .unwrap_or(self.interval * 2)
            .clamp(self.interval, MAX_BACKOFF.max(self.interval));

        if !self.connected {
            self.since = now;
        }

        self.connected = true;
        self.ever_connected = true;
        self.failures = 0;
        self.last_error = None;
        self.rate_limited = self.rate_limited.saturating_add(1);
        self.next_attempt = now + delay;

        info!(
            source = %self.name,
            seconds = delay.as_secs(),
            "The FIRMS source asked us to wait before the next request.",
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEN_MINUTES: Duration = Duration::from_secs(600);

    fn now() -> DateTime<Utc> {
        "2026-09-22T14:00:00Z".parse().expect("an instant")
    }

    #[test]
    fn a_source_is_asked_once_an_interval_whatever_the_tick_is() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());

        assert!(state.ready_at(now()), "a new source may be asked at once");

        state.succeeded_at(now());

        assert!(!state.ready_at(now() + chrono::Duration::minutes(9)));
        assert!(state.ready_at(now() + chrono::Duration::minutes(10)));
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
        assert!(!state.ready_at(now() + chrono::Duration::minutes(29)));
        assert!(state.ready_at(now() + chrono::Duration::minutes(30)));

        // A delay shorter than the interval never speeds the source up.
        state.wait_for_at(Some(Duration::from_secs(5)), now());

        assert!(!state.ready_at(now() + chrono::Duration::minutes(9)));
    }
}
