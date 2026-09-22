//! Saying a thing once, and counting the rest of them.
//!
//! # Why a state machine and not a log line
//!
//! The first live deployment of this plugin polled adsb.lol every five seconds
//! and was answered `429`, `Retry-After: 10`, on about every other request:
//! twelve identical "asked us to wait" notices in five minutes. The operator of
//! that deployment had retired a previous TAK server partly for log noise.
//!
//! So a thing that keeps happening is a **run**, not a series of events, and it
//! is announced the way `rustak-client`'s control link announces an outage: the
//! first occurrence carries the cause, everything after it is `debug`, every
//! [`REMIND_EVERY`] one line says how many there have been since the last one,
//! and the end of the run is a single line naming how long it lasted. A run of
//! any length costs a handful of lines, whatever happens inside it.
//!
//! # When a run is over
//!
//! Not every run ends the moment something works. An outage does — one request
//! that answers *is* the end of it — but a provider that refuses every other
//! request would then start a new run every other request, which is exactly the
//! noise this exists to stop. [`Repeated::new`] therefore takes how long the
//! thing has to *stop* happening before the run counts as finished.

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
#[derive(Clone, Debug)]
pub struct Repeated {
    /// How long it has to stop happening before the run is over.
    settle: Duration,

    /// When the current run started; [`None`] while nothing is happening.
    since: Option<DateTime<Utc>>,

    /// When it last happened.
    last: Option<DateTime<Utc>>,

    /// When this run was last mentioned above `debug`.
    mentioned_at: Option<DateTime<Utc>>,

    /// How many there have been in this run.
    count: u64,

    /// How many of those are since the last mention.
    unmentioned: u64,
}

impl Repeated {
    /// A run of nothing, which is over once `settle` has passed without it.
    ///
    /// [`Duration::ZERO`] is the outage case: anything that works ends it.
    #[must_use]
    pub const fn new(settle: Duration) -> Self {
        Self {
            settle,
            since: None,
            last: None,
            mentioned_at: None,
            count: 0,
            unmentioned: 0,
        }
    }

    /// Records one occurrence, and answers what to say about it.
    pub fn happened(&mut self, now: DateTime<Utc>) -> Report {
        self.count = self.count.saturating_add(1);
        self.unmentioned = self.unmentioned.saturating_add(1);
        self.last = Some(now);

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

    /// Records that it did not happen, and answers what to say about that.
    ///
    /// [`Report::Recovered`] once, at the end of a run that has settled;
    /// [`Report::Quiet`] whenever there was no run, or the run is still going.
    pub fn cleared(&mut self, now: DateTime<Utc>) -> Report {
        let (Some(since), Some(last)) = (self.since, self.last) else {
            return Report::Quiet;
        };

        if elapsed(last, now) < self.settle {
            return Report::Quiet;
        }

        let report = Report::Recovered {
            count: self.count,
            over: elapsed(since, now),
        };

        *self = Self::new(self.settle);

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

    fn outage() -> Repeated {
        Repeated::new(Duration::ZERO)
    }

    #[test]
    fn the_first_occurrence_of_a_run_is_the_one_that_is_announced() {
        let mut run = outage();

        assert_eq!(run.happened(at(0)), Report::First);
        assert!(run.standing());
    }

    #[test]
    fn every_occurrence_after_it_is_quiet_until_the_reminder_falls_due() {
        // The production finding: twelve identical notices in five minutes.
        let mut run = outage();

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
        let mut run = outage();

        run.happened(at(0));

        for second in 1..=22 {
            run.happened(at(second));
        }

        assert_eq!(
            run.happened(at(300)),
            Report::Reminder {
                count: 23,
                over: Duration::from_secs(300),
            },
            "the twenty-two inside the window, and this one",
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
        let mut run = outage();

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
    fn a_run_that_never_started_says_nothing_when_it_does_not_happen() {
        // Otherwise every successful poll of every healthy source is a line.
        let mut run = outage();

        assert_eq!(run.cleared(at(0)), Report::Quiet);
        assert_eq!(run.cleared(at(30)), Report::Quiet);
    }

    #[test]
    fn a_second_run_is_announced_again_rather_than_folded_into_the_first() {
        let mut run = outage();

        run.happened(at(0));
        run.cleared(at(10));

        assert_eq!(run.happened(at(20)), Report::First);
    }

    #[test]
    fn a_run_that_has_to_settle_is_not_over_the_first_time_it_does_not_happen() {
        // adsb.lol answered 429 on about every other request: a run that ended
        // on the next success would be a new announcement every other request,
        // which is the noise this whole module exists to stop.
        let mut run = Repeated::new(REMIND_EVERY);

        assert_eq!(run.happened(at(0)), Report::First);
        assert_eq!(run.cleared(at(10)), Report::Quiet, "ten seconds is a gap");
        assert_eq!(
            run.happened(at(20)),
            Report::Quiet,
            "and still the same run"
        );
        assert_eq!(run.cleared(at(200)), Report::Quiet);

        assert_eq!(
            run.cleared(at(330)),
            Report::Recovered {
                count: 2,
                over: Duration::from_secs(330),
            },
            "five minutes without it is the run being over",
        );
    }

    #[test]
    fn a_clock_that_stepped_backwards_is_not_a_negative_run() {
        let mut run = outage();

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
