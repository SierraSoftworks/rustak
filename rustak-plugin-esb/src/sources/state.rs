//! What an upstream is doing, and when it is worth asking again.
//!
//! A floor under the request rate that is independent of the sidecar's tick, a
//! capped backoff, and enough memory to tell an administrator whether the feed
//! is fine, struggling, or has never worked. It logs **changes** of state and
//! nothing else, so an outage of the upstream is two lines however long it is.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

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
}

impl SourceState {
    /// A source that has not been asked anything yet, and may be asked now.
    #[must_use]
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self {
            name: name.into(),
            interval,
            connected: false,
            failures: 0,
            since: Utc::now(),
            last_success: None,
            last_error: None,
            next_attempt: Utc::now(),
        }
    }

    /// The upstream's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often the upstream is asked.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether it is time to reach out again.
    #[must_use]
    pub fn ready(&self) -> bool {
        Utc::now() >= self.next_attempt
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
        let now = Utc::now();

        if !self.connected {
            info!(source = %self.name, failures = self.failures, "{} is answering.", self.name);
            self.since = now;
        }

        self.connected = true;
        self.failures = 0;
        self.last_success = Some(now);
        self.last_error = None;
        self.next_attempt = now + self.interval;
    }

    /// Records a failure, and backs off. `error` must be fit for an
    /// administrator to read; nothing here should ever be handed a credential.
    pub fn failed(&mut self, error: impl Into<String>) {
        let (now, error) = (Utc::now(), error.into());

        if self.connected || self.failures == 0 {
            warn!(source = %self.name, "{} stopped answering; retrying with backoff. {error}", self.name);
            self.since = now;
        } else {
            debug!(source = %self.name, "{} is still not answering. {error}", self.name);
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();
    }

    /// Holds off because the upstream said "not so fast", which is an upstream
    /// that is working: the connection state is left alone.
    ///
    /// A stated delay is honoured up to [`MAX_RETRY_AFTER`]; without one the
    /// guess is twice the interval. Answers how long the wait is, so a source
    /// can hold its other requests back for as long.
    pub fn wait_for(&mut self, asked: Option<Duration>) -> Duration {
        let delay = asked
            .map_or((self.interval * 2).min(MAX_BACKOFF), |stated| {
                stated.min(MAX_RETRY_AFTER)
            })
            .max(self.interval);

        // Said as what happened: a wait ESB named is ESB's, and one it did not
        // name is our own guess and must not be attributed to it.
        if asked.is_some() {
            info!(source = %self.name, seconds = delay.as_secs(), "{} asked us to wait {}s.", self.name, delay.as_secs());
        } else {
            info!(source = %self.name, seconds = delay.as_secs(), "{} refused a request (429) without naming a delay; waiting {}s.", self.name, delay.as_secs());
        }

        self.next_attempt = Utc::now() + delay;

        delay
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stated_delay_is_believed_only_so_far() {
        let week = Duration::from_secs(7 * 24 * 3600);

        assert_eq!(state().wait_for(Some(week)), MAX_RETRY_AFTER);
    }

    fn state() -> SourceState {
        SourceState::new("PowerCheck", Duration::from_secs(300))
    }

    #[test]
    fn a_new_source_may_be_asked_at_once_and_has_never_answered() {
        let state = state();

        assert!(state.ready());
        assert!(!state.is_connected());
        assert_eq!(state.last_success(), None);
    }

    #[test]
    fn an_answer_holds_the_next_request_off_for_an_interval() {
        let mut state = state();

        state.succeeded();

        assert!(state.is_connected());
        assert!(!state.ready());
        assert!(state.last_success().is_some());
    }

    #[test]
    fn a_failure_is_remembered_and_backed_off_from() {
        let mut state = state();
        state.succeeded();

        state.failed("connection refused");

        assert!(!state.is_connected());
        assert_eq!(state.last_error(), Some("connection refused"));
        assert!(state.last_success().is_some(), "it did work once");
        assert!(!state.ready());
    }

    #[test]
    fn being_asked_to_wait_is_not_a_failure() {
        let mut state = state();
        state.succeeded();

        let wait = state.wait_for(Some(Duration::from_secs(600)));

        assert_eq!(
            wait,
            Duration::from_secs(600),
            "longer than any backoff of ours"
        );
        assert!(state.is_connected());
        assert_eq!(state.last_error(), None);
        assert!(!state.ready());
    }
}
