//! Doing a thing no more often than every so often, on the caller's clock.
//!
//! A feed says what it has been doing in one `info` line every
//! [`REPORT_EVERY`] — [`FeedPublisher`](crate::feed::FeedPublisher)'s
//! `The feed is publishing.`, and the equivalent line of a plugin that
//! publishes through something of its own. The line is the plugin's; when it
//! is due is this.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// How often a feed's counters are logged at `info`. Slow on purpose: a feed
/// that logged its rate every tick would be the noisiest thing in the journal.
pub const REPORT_EVERY: Duration = Duration::from_secs(300);

/// Due the first time it is asked, and then once a period after the last time
/// it was due.
///
/// It is asked from a tick that already happens, so it costs no timer, and it
/// takes the instant to judge against, so a test moves the clock by hand.
#[derive(Clone, Copy, Debug)]
pub struct Every {
    period: Duration,
    last: Option<DateTime<Utc>>,
}

impl Every {
    /// Due at once, and then every `period`.
    #[must_use]
    pub const fn new(period: Duration) -> Self {
        Self { period, last: None }
    }

    /// Whether it is due at `now`; when it is, the next one is a period away.
    pub fn due_at(&mut self, now: DateTime<Utc>) -> bool {
        let due = self
            .last
            .is_none_or(|last| (now - last).to_std().unwrap_or_default() >= self.period);

        if due {
            self.last = Some(now);
        }

        due
    }
}

impl Default for Every {
    /// Every [`REPORT_EVERY`].
    fn default() -> Self {
        Self::new(REPORT_EVERY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    #[test]
    fn it_is_due_at_once_and_then_every_five_minutes() {
        let mut every = Every::default();

        assert!(every.due_at(at(0)), "the first tick says where it is");
        assert!(!every.due_at(at(1)));
        assert!(!every.due_at(at(299)), "not every tick");
        assert!(every.due_at(at(300)));
        assert!(!every.due_at(at(599)));
        assert!(every.due_at(at(600)));
    }

    #[test]
    fn a_late_tick_starts_the_next_period_from_itself() {
        let mut every = Every::new(Duration::from_secs(60));

        assert!(every.due_at(at(0)));
        assert!(every.due_at(at(90)), "late, and due");
        assert!(
            !every.due_at(at(120)),
            "a period from the late one, not from 0"
        );
        assert!(every.due_at(at(150)));
    }

    #[test]
    fn a_clock_that_stepped_backwards_is_not_due() {
        let mut every = Every::default();

        assert!(every.due_at(at(600)));
        assert!(!every.due_at(at(0)));
    }
}
