//! What an upstream is doing, and when it is worth asking again.
//!
//! Every live source in this plugin is an HTTP GET on a timer, so all three
//! want the same three things: a floor under how often they reach out, a capped
//! backoff when the answer is a failure, and enough memory of what happened to
//! tell an administrator whether the feed is fine, struggling or has never
//! worked at all. That is this type, and it is deliberately the only thing in
//! the crate that logs a *state change* — a source that logged every failed
//! poll would fill a log with the same line every five seconds.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

/// The longest a source waits between attempts, however many have failed.
///
/// Five minutes: long enough that a receiver that has been unplugged for the
/// weekend is not a request every second, short enough that plugging it back in
/// puts aircraft on the map while somebody is still standing next to it.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How many doublings the backoff is allowed, before [`MAX_BACKOFF`] catches
/// it anyway. Six, so the shift cannot overflow on a source that has been
/// failing for a week.
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
    next_attempt: Instant,
}

impl SourceState {
    /// A source that has not been asked anything yet, and may be asked now.
    #[must_use]
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self {
            name: name.into(),
            interval,
            connected: false,
            ever_connected: false,
            failures: 0,
            since: Utc::now(),
            last_error: None,
            next_attempt: Instant::now(),
        }
    }

    /// The upstream's name, for a log line or a heartbeat.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often this source may reach its upstream.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether it is time to reach out again.
    ///
    /// A source polls on the sidecar's tick, which may be far more often than
    /// its upstream wants to be asked; this is what makes the two independent.
    #[must_use]
    pub fn ready(&self) -> bool {
        Instant::now() >= self.next_attempt
    }

    /// Records an answer, and schedules the next attempt one interval away.
    pub fn succeeded(&mut self) {
        if !self.connected {
            info!(
                source = %self.name,
                "The ADS-B source answered; the feed is connected.",
            );
            self.since = Utc::now();
        }

        self.connected = true;
        self.ever_connected = true;
        self.failures = 0;
        self.last_error = None;
        self.next_attempt = Instant::now() + self.interval;
    }

    /// Records a failure, and backs off.
    ///
    /// `error` is already a message fit for an administrator to read: nothing
    /// here redacts, because nothing here should ever be handed a credential.
    pub fn failed(&mut self, error: impl Into<String>) {
        let error = error.into();

        if self.connected || self.failures == 0 {
            warn!(
                source = %self.name,
                "The ADS-B source stopped answering; retrying with backoff. {error}",
            );
            self.since = Utc::now();
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = Instant::now() + self.backoff();
    }

    /// Holds off for a stated delay, which is what a `429` asks for.
    ///
    /// Not a failure: an upstream saying "not so fast" is one that is working,
    /// so the connection state is left alone and only the schedule moves.
    pub fn wait_for(&mut self, delay: Duration) {
        let delay = delay.min(MAX_BACKOFF);

        info!(
            source = %self.name,
            seconds = delay.as_secs(),
            "The ADS-B source asked us to wait before the next request.",
        );

        self.next_attempt = Instant::now() + delay.max(self.interval);
    }

    /// Whether the last attempt worked.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Whether any attempt has ever worked.
    ///
    /// The difference between "this feed is having a bad afternoon" and "this
    /// feed has never worked", which is a configuration error rather than an
    /// outage — and the difference between `degraded` and `unhealthy`.
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

    fn state() -> SourceState {
        SourceState::new("adsb.lol", Duration::from_secs(5))
    }

    #[test]
    fn a_new_source_is_ready_and_has_never_connected() {
        let state = state();

        assert!(state.ready(), "the first poll happens immediately");
        assert!(!state.is_connected());
        assert!(!state.ever_connected());
        assert_eq!(state.last_error(), None);
        assert_eq!(state.interval(), Duration::from_secs(5));
    }

    #[test]
    fn a_successful_poll_holds_the_next_one_off_for_an_interval() {
        let mut state = state();

        state.succeeded();

        assert!(state.is_connected());
        assert!(state.ever_connected());
        assert!(!state.ready(), "five seconds have not passed");
        assert_eq!(state.reconnecting_for(), None);
    }

    #[test]
    fn the_backoff_doubles_and_then_stops_doubling() {
        let mut state = state();

        for (failures, expected) in [
            (1_u32, Duration::from_secs(5)),
            (2, Duration::from_secs(10)),
            (3, Duration::from_secs(20)),
            (4, Duration::from_secs(40)),
            (5, Duration::from_secs(80)),
            (6, Duration::from_secs(160)),
            (7, MAX_BACKOFF),
            (80, MAX_BACKOFF),
        ] {
            state.failures = failures;

            assert_eq!(state.backoff(), expected, "after {failures} failures");
        }
    }

    #[test]
    fn a_failure_remembers_what_went_wrong_and_that_it_never_worked() {
        let mut state = state();

        state.failed("the receiver refused the connection");

        assert!(!state.is_connected());
        assert!(!state.ever_connected(), "it has never answered");
        assert_eq!(
            state.last_error(),
            Some("the receiver refused the connection"),
        );
        assert!(state.reconnecting_for().is_some());
        assert!(!state.ready());
    }

    #[test]
    fn recovering_clears_the_failure_and_the_backoff() {
        let mut state = state();

        state.failed("timed out");
        state.failed("timed out");
        state.succeeded();

        assert_eq!(state.failures, 0);
        assert_eq!(state.last_error(), None);
        assert!(state.is_connected());
    }

    #[test]
    fn a_rate_limit_moves_the_schedule_without_marking_a_failure() {
        let mut state = state();
        state.succeeded();

        state.wait_for(Duration::from_secs(60));

        assert!(state.is_connected(), "429 is an upstream that is working");
        assert_eq!(state.last_error(), None);
        assert!(!state.ready());
    }

    #[test]
    fn a_rate_limit_never_asks_us_to_wait_longer_than_the_cap() {
        let mut state = state();

        state.wait_for(Duration::from_secs(86_400));

        // A `Retry-After` of a day is either a mistake or a ban; either way a
        // sidecar that stopped polling until tomorrow would never notice it
        // being lifted.
        assert!(state.next_attempt <= Instant::now() + MAX_BACKOFF);
    }

    #[test]
    fn a_short_rate_limit_still_respects_the_configured_interval() {
        let mut state = SourceState::new("adsb.fi", Duration::from_secs(5));

        state.wait_for(Duration::from_millis(100));

        assert!(state.next_attempt >= Instant::now() + Duration::from_secs(4));
    }
}
