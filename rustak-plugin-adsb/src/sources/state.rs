//! What an upstream is doing, and when it is worth asking again.
//!
//! Every live source in this plugin is an HTTP GET on a timer, so all three
//! want the same three things: a floor under how often they reach out, a capped
//! backoff when the answer is a failure, and enough memory of what happened to
//! tell an administrator whether the feed is fine, struggling or has never
//! worked at all. That is this type, and it is deliberately the only thing in
//! the crate that logs a *state change* — a source that logged every failed
//! poll would fill a log with the same line every five seconds.
//!
//! Each of those runs — the outage, the rate limiting — is a [`Repeated`], so
//! it costs one line at the start, one every five minutes while it lasts and
//! one at the end, however many polls it spans.
//!
//! # The cadence adapts to the provider
//!
//! adsb.lol answered the first live deployment `429`, `Retry-After: 10`, on
//! about every other request at a five-second poll. Waiting the ten seconds out
//! and then going straight back to five is asking to be refused again, so a
//! provider that says that **twice inside [`LIMIT_WINDOW`] polls** is taken at
//! its word: the interval becomes what it asked for, for the rest of the
//! process. It is never lowered again — a service that has told us twice how
//! often it wants to be asked has earned the benefit of the doubt until
//! somebody restarts the sidecar having read this.
//!
//! Only a delay the provider *stated* moves the cadence. A `429` with no
//! `Retry-After` is waited out on a guess of our own, and a number we made up
//! is not a number to make permanent.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::notice::{Repeated, Report, humanised};

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

/// How many polls apart two rate limits may be and still be the provider
/// telling us how often it wants to be asked, rather than two bad minutes.
///
/// Ten: at any sane interval that is a minute or two of asking, which is short
/// enough that an unrelated pair does not slow a feed down for the rest of the
/// day and long enough that "every other request" is caught on the second one.
pub const LIMIT_WINDOW: u64 = 10;

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

    /// The run of failures, so an outage is announced once.
    outage: Repeated,

    /// The run of rate limits. It settles rather than ending on the next
    /// request that works, because a provider refusing every other request is
    /// one run and not a hundred.
    limit: Repeated,

    /// How many attempts have been made, which is what [`LIMIT_WINDOW`] counts.
    polls: u64,

    /// The attempt the last stated `Retry-After` arrived on.
    limited_at: Option<u64>,

    /// How many `429`s this source has been sent, for the heartbeat.
    rate_limited: u64,
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
            next_attempt: Utc::now(),
            outage: Repeated::new(Duration::ZERO),
            limit: Repeated::new(super::notice::REMIND_EVERY),
            polls: 0,
            limited_at: None,
            rate_limited: 0,
        }
    }

    /// The upstream's name, for a log line or a heartbeat.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often this source may reach its upstream **as it stands**: the
    /// configured interval, or whatever a provider has since asked for.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// How many times this source has been rate-limited, for the heartbeat.
    #[must_use]
    pub const fn rate_limited(&self) -> u64 {
        self.rate_limited
    }

    /// Whether it is time to reach out again.
    ///
    /// A source polls on the sidecar's tick, which may be far more often than
    /// its upstream wants to be asked; this is what makes the two independent.
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
        self.polls = self.polls.saturating_add(1);

        match self.outage.cleared(now) {
            Report::Recovered { count, over } => {
                info!(
                    source = %self.name,
                    "The ADS-B source answered again after {} and {count} failed attempts; \
                     the feed is connected.",
                    humanised(over),
                );
                self.since = now;
            }
            _ if !self.ever_connected => {
                info!(
                    source = %self.name,
                    "The ADS-B source answered; the feed is connected.",
                );
                self.since = now;
            }
            _ => {}
        }

        if let Report::Recovered { count, over } = self.limit.cleared(now) {
            info!(
                source = %self.name,
                "The ADS-B source has stopped asking us to wait; {count} requests were held \
                 back over {}.",
                humanised(over),
            );
        }

        self.connected = true;
        self.ever_connected = true;
        self.failures = 0;
        self.last_error = None;
        self.next_attempt = now + self.interval;
    }

    /// Records a failure, and backs off.
    ///
    /// `error` is already a message fit for an administrator to read: nothing
    /// here redacts, because nothing here should ever be handed a credential.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.failed_at(error, Utc::now());
    }

    /// [`failed`](Self::failed), at an instant of the caller's choosing.
    pub fn failed_at(&mut self, error: impl Into<String>, now: DateTime<Utc>) {
        let error = error.into();
        self.polls = self.polls.saturating_add(1);

        match self.outage.happened(now) {
            Report::First => {
                self.since = now;
                warn!(
                    source = %self.name,
                    "The ADS-B source stopped answering; retrying with backoff. {error}",
                );
            }
            Report::Reminder { count, over } => warn!(
                source = %self.name,
                "The ADS-B source has not answered for {}; {count} attempts failed in the last \
                 {}. {error}",
                humanised(elapsed(self.since, now)),
                humanised(over),
            ),
            _ => debug!(
                source = %self.name,
                "The ADS-B source is still not answering. {error}",
            ),
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();
    }

    /// Holds off because the upstream said "not so fast".
    ///
    /// `asked` is the delay the response stated, when it stated one. Not a
    /// failure: an upstream saying "not so fast" is one that is working, so the
    /// connection state is left alone and only the schedule moves.
    pub fn wait_for(&mut self, asked: Option<Duration>) {
        self.wait_for_at(asked, Utc::now());
    }

    /// [`wait_for`](Self::wait_for), at an instant of the caller's choosing.
    pub fn wait_for_at(&mut self, asked: Option<Duration>, now: DateTime<Utc>) {
        // A `429` that names no delay is waited out on twice the interval,
        // which is our own guess and therefore never moves the cadence.
        let delay = asked.unwrap_or(self.interval * 2).min(MAX_BACKOFF);

        self.polls = self.polls.saturating_add(1);
        self.rate_limited = self.rate_limited.saturating_add(1);

        if let Some(raised) = asked.and_then(|stated| self.adapt(stated.min(MAX_BACKOFF))) {
            info!(
                source = %self.name,
                seconds = raised.as_secs(),
                "{} asks for {}s between requests; polling at that rate from now on.",
                self.name,
                raised.as_secs(),
            );
        }

        match self.limit.happened(now) {
            Report::First => info!(
                source = %self.name,
                seconds = delay.as_secs(),
                "The ADS-B source asked us to wait before the next request.",
            ),
            Report::Reminder { count, over } => info!(
                source = %self.name,
                "The ADS-B source asked us to wait {count} times in the last {}.",
                humanised(over),
            ),
            _ => debug!(
                source = %self.name,
                seconds = delay.as_secs(),
                "The ADS-B source asked us to wait again.",
            ),
        }

        self.next_attempt = now + delay.max(self.interval);
    }

    /// Takes a provider at its word once it has said the same thing twice
    /// inside [`LIMIT_WINDOW`] polls, and answers the interval it raised to.
    fn adapt(&mut self, stated: Duration) -> Option<Duration> {
        let asked_again = self
            .limited_at
            .is_some_and(|at| self.polls.saturating_sub(at) <= LIMIT_WINDOW);

        self.limited_at = Some(self.polls);

        // `max`, never `min`: a provider that has told us twice is not talked
        // back down by one that asks for less later, nor by the clock.
        if !asked_again || stated <= self.interval {
            return None;
        }

        self.interval = stated;

        Some(stated)
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
        let mut state = SourceState::new("adsb.lol", Duration::from_secs(5));
        state.since = at(0);
        state.next_attempt = at(0);

        state
    }

    #[test]
    fn a_new_source_is_ready_and_has_never_connected() {
        let state = state();

        assert!(state.ready_at(at(0)), "the first poll happens immediately");
        assert!(!state.is_connected());
        assert!(!state.ever_connected());
        assert_eq!(state.last_error(), None);
        assert_eq!(state.interval(), Duration::from_secs(5));
        assert_eq!(state.rate_limited(), 0);
    }

    #[test]
    fn a_successful_poll_holds_the_next_one_off_for_an_interval() {
        let mut state = state();

        state.succeeded_at(at(0));

        assert!(state.is_connected());
        assert!(state.ever_connected());
        assert!(!state.ready_at(at(4)), "five seconds have not passed");
        assert!(state.ready_at(at(5)));
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

        state.failed_at("the receiver refused the connection", at(0));

        assert!(!state.is_connected());
        assert!(!state.ever_connected(), "it has never answered");
        assert_eq!(
            state.last_error(),
            Some("the receiver refused the connection"),
        );
        assert!(state.reconnecting_for().is_some());
        assert!(!state.ready_at(at(4)));
    }

    #[test]
    fn recovering_clears_the_failure_and_the_backoff() {
        let mut state = state();

        state.failed_at("timed out", at(0));
        state.failed_at("timed out", at(10));
        state.succeeded_at(at(30));

        assert_eq!(state.failures, 0);
        assert_eq!(state.last_error(), None);
        assert!(state.is_connected());
        assert!(!state.outage.standing(), "and the run is over");
    }

    #[test]
    fn a_rate_limit_moves_the_schedule_without_marking_a_failure() {
        let mut state = state();
        state.succeeded_at(at(0));

        state.wait_for_at(Some(Duration::from_secs(60)), at(5));

        assert!(state.is_connected(), "429 is an upstream that is working");
        assert_eq!(state.last_error(), None);
        assert_eq!(state.rate_limited(), 1);
        assert!(!state.ready_at(at(64)));
        assert!(state.ready_at(at(65)));
    }

    #[test]
    fn a_rate_limit_never_asks_us_to_wait_longer_than_the_cap() {
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(86_400)), at(0));

        // A `Retry-After` of a day is either a mistake or a ban; either way a
        // sidecar that stopped polling until tomorrow would never notice it
        // being lifted.
        assert!(state.ready_at(at(0) + MAX_BACKOFF));
    }

    #[test]
    fn a_short_rate_limit_still_respects_the_configured_interval() {
        let mut state = SourceState::new("adsb.fi", Duration::from_secs(5));

        state.wait_for_at(Some(Duration::from_millis(100)), at(0));

        assert!(!state.ready_at(at(4)));
    }

    #[test]
    fn two_stated_rate_limits_inside_ten_polls_raise_the_interval_for_good() {
        // The Dublin finding: 429 with Retry-After: 10 on about every other
        // request at a five-second poll.
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(10)), at(0));

        assert_eq!(
            state.interval(),
            Duration::from_secs(5),
            "one 429 is a bad minute, not a rate limit",
        );

        state.succeeded_at(at(10));
        state.wait_for_at(Some(Duration::from_secs(10)), at(20));

        assert_eq!(
            state.interval(),
            Duration::from_secs(10),
            "the second one inside the window is the provider telling us its rate",
        );
        assert_eq!(state.rate_limited(), 2);

        // And it is never talked back down, by a smaller ask or by the clock.
        state.succeeded_at(at(30));
        state.wait_for_at(Some(Duration::from_secs(1)), at(40));
        state.wait_for_at(Some(Duration::from_secs(1)), at(50));

        assert_eq!(state.interval(), Duration::from_secs(10));
    }

    #[test]
    fn two_rate_limits_further_apart_than_the_window_are_two_bad_minutes() {
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(10)), at(0));

        for second in 1..=(LIMIT_WINDOW as i64 + 1) {
            state.succeeded_at(at(second * 10));
        }

        state.wait_for_at(Some(Duration::from_secs(10)), at(500));

        assert_eq!(
            state.interval(),
            Duration::from_secs(5),
            "eleven polls apart is not a provider asking for a slower cadence",
        );
    }

    #[test]
    fn a_rate_limit_with_no_retry_after_waits_but_never_moves_the_cadence() {
        // The delay is our own guess; making a guess permanent is how a feed
        // slows itself to a crawl over a week.
        let mut state = state();

        state.wait_for_at(None, at(0));
        state.wait_for_at(None, at(20));

        assert_eq!(state.interval(), Duration::from_secs(5));
        assert!(!state.ready_at(at(29)), "it still waits twice the interval");
        assert!(state.ready_at(at(30)));
    }

    #[test]
    fn a_provider_that_refuses_every_other_request_is_one_run_of_notices() {
        // What the operator sees, rather than what the plugin does: twelve
        // notices in five minutes was the complaint.
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(10)), at(0));

        assert!(state.limit.standing());

        for poll in 1..24 {
            state.succeeded_at(at(poll * 10));
            state.wait_for_at(Some(Duration::from_secs(10)), at(poll * 10 + 5));
        }

        assert!(
            state.limit.standing(),
            "a success in between does not end a run of rate limiting",
        );

        // Five minutes of not being refused is the end of it.
        state.succeeded_at(at(1_000));

        assert!(!state.limit.standing());
    }
}
