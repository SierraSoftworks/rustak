//! What a polled upstream is doing, and when it is worth asking again.

use std::marker::PhantomData;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use super::{REMIND_EVERY, Repeated, Report, Rules};

/// How many doublings the backoff is allowed before
/// [`Rules::MAX_BACKOFF`] catches it anyway, so the shift cannot overflow on
/// a source that has failed for a week.
const MAX_DOUBLINGS: u32 = 6;

/// How a polled upstream is doing: a floor under the request rate that is
/// independent of the sidecar's tick, a capped backoff, `Retry-After`
/// honoured above the interval, and enough memory to tell an administrator
/// whether the feed is fine, struggling, or has never worked.
///
/// It is the one place a source logs a **state change**, in `R`'s words. Each
/// of the two things that go on for a while — the upstream not answering, and
/// the upstream saying "not so fast" — is a [`Repeated`] run: one line when it
/// starts, one every [`REMIND_EVERY`] while it lasts, one when it is over, and
/// `debug` for every attempt between.
///
/// Every method that moves it has an `_at` form taking the instant to judge
/// against, so a suite drives it on a clock it moves by hand.
#[derive(Clone, Debug)]
pub struct SourceState<R: Rules> {
    name: String,
    interval: Duration,
    connected: bool,
    ever_connected: bool,
    failures: u32,
    since: DateTime<Utc>,
    last_success: Option<DateTime<Utc>>,
    last_error: Option<String>,
    next_attempt: DateTime<Utc>,
    rate_limited: u64,

    /// The run of failures, so an outage is announced once.
    outage: Repeated,

    /// The run of refusals. It settles rather than ending on the next answer,
    /// so a provider refusing every other request is one run and not many.
    limit: Repeated,

    rules: PhantomData<R>,
}

impl<R: Rules> SourceState<R> {
    /// A source that has not been asked anything yet, and may be asked now.
    #[must_use]
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self::new_at(name, interval, Utc::now())
    }

    /// [`new`](Self::new), at an instant of the caller's choosing.
    #[must_use]
    pub fn new_at(name: impl Into<String>, interval: Duration, now: DateTime<Utc>) -> Self {
        let settle = REMIND_EVERY.max(interval.saturating_mul(R::REFUSALS_SETTLE_OVER_POLLS));

        Self {
            name: name.into(),
            interval,
            connected: false,
            ever_connected: false,
            failures: 0,
            since: now,
            last_success: None,
            last_error: None,
            next_attempt: now,
            rate_limited: 0,
            outage: Repeated::new(Duration::ZERO),
            limit: Repeated::new(settle),
            rules: PhantomData,
        }
    }

    /// The upstream's name, for a log line or a heartbeat.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often the upstream is asked, at the fastest.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// Moves the interval, for a plugin whose cadence adapts to its provider.
    ///
    /// This type never moves it on its own: `poll` is a floor, a stated
    /// `Retry-After` is waited out above it once, and the next answer is an
    /// interval away again. A plugin that wants a raised floor to stay raised
    /// (ADS-B's) keeps that decision and sets the result here before each
    /// `succeeded`/`failed`/`wait_for`, which then schedule from it.
    pub const fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// How many times the upstream has said "not so fast".
    #[must_use]
    pub const fn rate_limited(&self) -> u64 {
        self.rate_limited
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

    /// When the next request is due.
    #[must_use]
    pub const fn next_attempt(&self) -> DateTime<Utc> {
        self.next_attempt
    }

    /// Makes the next request due at `at`, keeping the interval and the
    /// backoff that follows from it. For a suite that will not wait five
    /// minutes for a poll; a plugin that calls it on a live upstream is
    /// overriding whatever that upstream asked for.
    pub const fn due_at(&mut self, at: DateTime<Utc>) {
        self.next_attempt = at;
    }

    /// Records an answer, and schedules the next request an interval away.
    pub fn succeeded(&mut self) {
        self.succeeded_at(Utc::now());
    }

    /// [`succeeded`](Self::succeeded), at an instant of the caller's choosing.
    /// Answers what was said about the outage run and the rate-limit run, in
    /// that order.
    pub fn succeeded_at(&mut self, now: DateTime<Utc>) -> (Report, Report) {
        let outage = self.answered(now);
        let limit = self.limit.cleared(now);

        if let Report::Recovered { count, over } = limit {
            info!(source = %self.name, "{}", R::stopped_refusing(&self.name, count, over));
        }

        self.last_success = Some(now);
        self.next_attempt = now + self.interval;

        (outage, limit)
    }

    /// Records a failure, and backs off. `error` must already be fit for an
    /// administrator to read: nothing here redacts, so nothing here should
    /// ever be handed a credential.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.failed_at(error, Utc::now());
    }

    /// [`failed`](Self::failed), at an instant of the caller's choosing.
    /// Answers what was said about it.
    pub fn failed_at(&mut self, error: impl Into<String>, now: DateTime<Utc>) -> Report {
        let error = error.into();
        let report = self.outage.happened(now);
        let name = &self.name;

        match report {
            Report::First => {
                self.since = now;
                warn!(source = %name, "{}", R::stopped_answering(name, &error));
            }
            Report::Reminder { count, over } => warn!(
                source = %name,
                "{}",
                R::not_answering(name, elapsed(self.since, now), count, over, &error),
            ),
            _ => debug!(source = %name, "{}", R::still_not_answering(name, &error)),
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();

        report
    }

    /// Holds off because the upstream said "not so fast". `asked` is the delay
    /// the response stated, when it stated one.
    ///
    /// **`poll` is a floor, not a pin.** A stated delay is honoured above the
    /// interval, up to [`Rules::MAX_RETRY_AFTER`]; one that is not stated is
    /// waited out on twice the interval, up to [`Rules::MAX_BACKOFF`], and said
    /// to be our own guess. Never shorter than the interval, however short the
    /// delay asked. Whether this is also an answer is
    /// [`Rules::REFUSAL_IS_AN_ANSWER`].
    pub fn wait_for(&mut self, asked: Option<Duration>) -> Duration {
        self.wait_for_at(asked, Utc::now()).0
    }

    /// [`wait_for`](Self::wait_for), at an instant of the caller's choosing.
    /// Answers the wait, so a source can hold its other requests back for as
    /// long, and what was said about it.
    pub fn wait_for_at(
        &mut self,
        asked: Option<Duration>,
        now: DateTime<Utc>,
    ) -> (Duration, Report) {
        let every = self.interval;
        let delay = asked
            .map_or(every.saturating_mul(2).min(R::MAX_BACKOFF), |stated| {
                stated.min(R::MAX_RETRY_AFTER)
            })
            .max(every);

        if R::REFUSAL_IS_AN_ANSWER {
            self.answered(now);
        }

        self.rate_limited = self.rate_limited.saturating_add(1);
        self.next_attempt = now + delay;

        let report = self.limit.happened(now);
        let (name, seconds, stated) = (&self.name, delay.as_secs(), asked.is_some());

        match report {
            Report::First => {
                info!(source = %name, seconds, stated, "{}", R::refused(name, asked, delay));
            }
            Report::Reminder { count, over } => info!(
                source = %name,
                seconds,
                stated,
                "{}",
                R::still_refusing(name, count, over, every, delay),
            ),
            _ => {
                debug!(source = %name, seconds, stated, "{}", R::refused_again(name, asked, delay))
            }
        }

        (delay, report)
    }

    /// Records that the upstream answered at all, which ends an outage.
    fn answered(&mut self, now: DateTime<Utc>) -> Report {
        let report = self.outage.cleared(now);

        match report {
            Report::Recovered { count, over } => {
                info!(source = %self.name, "{}", R::answered_again(&self.name, count, over));
                self.since = now;
            }
            _ if !self.ever_connected => {
                info!(source = %self.name, "{}", R::answering(&self.name));
                self.since = now;
            }
            _ => {}
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

    /// When the upstream last answered with something other than a refusal.
    #[must_use]
    pub const fn last_success(&self) -> Option<DateTime<Utc>> {
        self.last_success
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

    /// How many attempts in a row have failed.
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }

    /// Whether an outage run is going on.
    #[must_use]
    pub const fn in_outage(&self) -> bool {
        self.outage.standing()
    }

    /// Whether a run of refusals is going on, which outlasts the refusals
    /// themselves until it settles.
    #[must_use]
    pub const fn being_refused(&self) -> bool {
        self.limit.standing()
    }

    /// The wait before the next attempt: the interval, doubled once per
    /// consecutive failure, capped at [`Rules::MAX_BACKOFF`] and never below
    /// the interval.
    fn backoff(&self) -> Duration {
        let doublings = self.failures.saturating_sub(1).min(MAX_DOUBLINGS);

        self.interval
            .saturating_mul(1_u32 << doublings)
            .min(R::MAX_BACKOFF)
            .max(self.interval)
    }
}

/// `now - before`, never negative.
fn elapsed(before: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - before).to_std().unwrap_or_default()
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
