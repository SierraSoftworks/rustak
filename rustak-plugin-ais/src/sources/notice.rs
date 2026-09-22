//! Saying a thing once, and counting the rest of them.
//!
//! # Why a state machine and not a log line
//!
//! An upstream that is down is down for a while. A source that logged every
//! attempt would write the same sentence every second for as long as it lasted,
//! and the operator of the first live deployment had retired a previous TAK
//! server partly for exactly that. So a thing that keeps happening is a **run**,
//! not a series of events: the first occurrence carries the cause, everything
//! after it is `debug`, every [`REMIND_EVERY`] one line says how many there have
//! been since the last one, and the end of the run is a single line naming how
//! long it lasted.
//!
//! It is the same shape as `rustak-client`'s control link and as
//! `rustak-plugin-adsb`'s own `sources::notice`, deliberately: an operator
//! reading a log with a server and two sidecars in it should not have to learn
//! three ways of being told the same thing. It is a copy rather than a shared
//! type because the only crate all three depend on is `rustak-client`, and a
//! sidecar's log cadence is not part of the SDK's contract with a plugin.
//!
//! Everything here is a run that ends the moment the thing works again — a
//! connection, a bound port — which is why there is nothing like the ADS-B
//! copy's settling period: this plugin has no upstream that refuses every
//! other request.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// How often a run that is still going is mentioned again.
pub const REMIND_EVERY: Duration = Duration::from_secs(300);

/// What one occurrence is worth saying out loud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// Nothing new: log it at `debug` and leave it there.
    Quiet,

    /// The first occurrence of a run — one line, with the cause.
    First,

    /// Still happening. One line: how many, over how long.
    Reminder {
        /// How many there have been since the last thing said about this run.
        count: u64,
        /// How long ago that was.
        over: Duration,
    },

    /// It has stopped. One line: how many there were, over how long.
    Recovered {
        /// How many there were in the whole run.
        count: u64,
        /// How long the run lasted.
        over: Duration,
    },
}

/// A thing that keeps happening, announced once and then counted.
///
/// Every method takes the instant to judge against, so the tests move the clock
/// by hand and nothing here waits.
#[derive(Clone, Debug, Default)]
pub struct Repeated {
    /// When the current run started; [`None`] while nothing is happening.
    since: Option<DateTime<Utc>>,

    /// When this run was last mentioned above `debug`.
    mentioned_at: Option<DateTime<Utc>>,

    /// How many there have been in this run.
    count: u64,

    /// How many of those are since the last mention.
    unmentioned: u64,
}

impl Repeated {
    /// Records one occurrence, and answers what to say about it.
    pub fn happened(&mut self, now: DateTime<Utc>) -> Report {
        self.count = self.count.saturating_add(1);
        self.unmentioned = self.unmentioned.saturating_add(1);

        if self.since.is_none() {
            self.since = Some(now);
            self.mentioned_at = Some(now);
            self.unmentioned = 0;

            return Report::First;
        }

        let quiet_for = self
            .mentioned_at
            .map_or(REMIND_EVERY, |at| elapsed(at, now));

        if quiet_for < REMIND_EVERY {
            return Report::Quiet;
        }

        self.mentioned_at = Some(now);

        Report::Reminder {
            count: std::mem::take(&mut self.unmentioned),
            over: quiet_for,
        }
    }

    /// Records that it is over, and answers what to say about that.
    ///
    /// [`Report::Recovered`] once, at the end of a run; [`Report::Quiet`]
    /// whenever there was no run to end — which is what stops a source that
    /// has never failed logging a line every time it works.
    pub fn cleared(&mut self, now: DateTime<Utc>) -> Report {
        let Some(since) = self.since.take() else {
            return Report::Quiet;
        };

        let report = Report::Recovered {
            count: self.count,
            over: elapsed(since, now),
        };

        *self = Self::default();

        report
    }

    /// Whether a run is going on.
    ///
    /// Nothing in the plugin branches on this — the [`Report`] is what a caller
    /// acts on — so it exists for the suites that assert a run is still the
    /// same run rather than a new one.
    #[cfg(test)]
    #[must_use]
    pub const fn standing(&self) -> bool {
        self.since.is_some()
    }
}

/// `now - before`, never negative — a clock that stepped backwards is not a
/// negative run.
fn elapsed(before: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - before).to_std().unwrap_or_default()
}

/// A duration as an operator reads it: "4m12s", "3s", "1h02m".
#[must_use]
pub fn humanised(duration: Duration) -> String {
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

    #[test]
    fn the_first_occurrence_of_a_run_is_the_one_that_is_announced() {
        let mut run = Repeated::default();

        assert_eq!(run.happened(at(0)), Report::First);
        assert!(run.standing());
    }

    #[test]
    fn every_occurrence_after_it_is_quiet_until_the_reminder_falls_due() {
        let mut run = Repeated::default();

        assert_eq!(run.happened(at(0)), Report::First);

        for second in [1, 3, 7, 15, 31, 91, 151, 240, 299] {
            assert_eq!(
                run.happened(at(second)),
                Report::Quiet,
                "the run was announced at 0s; {second}s must be quiet",
            );
        }
    }

    #[test]
    fn a_run_that_is_still_going_is_mentioned_once_every_five_minutes_with_a_count() {
        let mut run = Repeated::default();

        run.happened(at(0));

        for second in 1..=8 {
            run.happened(at(second * 30));
        }

        assert_eq!(
            run.happened(at(300)),
            Report::Reminder {
                count: 9,
                over: Duration::from_secs(300),
            },
        );
        assert_eq!(run.happened(at(400)), Report::Quiet);
        assert_eq!(
            run.happened(at(600)),
            Report::Reminder {
                count: 2,
                over: Duration::from_secs(300),
            },
            "the count is since the last reminder, not since the run started",
        );
    }

    #[test]
    fn the_end_of_a_run_names_how_many_there_were_and_how_long_it_lasted() {
        let mut run = Repeated::default();

        run.happened(at(0));
        run.happened(at(30));
        run.happened(at(60));

        assert_eq!(
            run.cleared(at(90)),
            Report::Recovered {
                count: 3,
                over: Duration::from_secs(90),
            },
        );
        assert!(!run.standing());
    }

    #[test]
    fn a_run_that_never_started_says_nothing_when_it_ends() {
        // Otherwise a source that has never failed logs a line every time it
        // connects, which for a stream that reopens cleanly is every reopen.
        let mut run = Repeated::default();

        assert_eq!(run.cleared(at(0)), Report::Quiet);
        assert_eq!(run.cleared(at(30)), Report::Quiet);
    }

    #[test]
    fn a_second_run_is_announced_again_rather_than_folded_into_the_first() {
        let mut run = Repeated::default();

        run.happened(at(0));
        run.cleared(at(10));

        assert_eq!(run.happened(at(20)), Report::First);
    }

    #[test]
    fn a_clock_that_stepped_backwards_is_not_a_negative_run() {
        let mut run = Repeated::default();

        run.happened(at(100));

        assert_eq!(
            run.cleared(at(40)),
            Report::Recovered {
                count: 1,
                over: Duration::ZERO,
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
