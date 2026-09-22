//! What a source says about being rate-limited, in one place.
//!
//! # Why the words are their own module
//!
//! The second live deployment logged `The ADS-B source asked us to wait before
//! the next request. seconds=20` against a provider that had asked for nothing:
//! adsb.lol answered `429` with no `Retry-After` at all, the twenty seconds was
//! this plugin's own fallback, and the line put our guess in the server's
//! mouth. An operator reading it concluded — reasonably — that the provider
//! wanted twenty seconds, and the status note before this one had concluded the
//! same thing from the same line.
//!
//! So the rule here is that **a sentence says who chose the number**. "Asked us
//! to wait" is only ever written about a delay the response stated; a refusal
//! that named none says so, and says that the wait is ours.
//!
//! They are functions that answer a [`String`] rather than `info!` calls at the
//! point of use so that the suites can assert the distinction without reading a
//! log, and so that the next person to reword one sees all of them at once.

use std::time::Duration;

use super::cadence::{CLEAN_RUN, Change};
use crate::sources::notice::humanised;

/// The first refusal of a run.
///
/// `asked` is what the response stated, when it stated anything; `waiting` is
/// what this source will actually wait, which is not always the same number —
/// a `Retry-After` shorter than the poll interval, or longer than the cap.
#[must_use]
pub fn refused(asked: Option<Duration>, waiting: Duration) -> String {
    match asked {
        Some(stated) if stated == waiting => format!(
            "The ADS-B source asked us to wait {} before the next request.",
            humanised(stated),
        ),
        Some(stated) => format!(
            "The ADS-B source asked us to wait {} before the next request; waiting {}.",
            humanised(stated),
            humanised(waiting),
        ),
        None => format!(
            "The ADS-B source refused a request (429) without naming a delay; waiting {} before \
             the next one.",
            humanised(waiting),
        ),
    }
}

/// Every refusal after the first, which is `debug`.
#[must_use]
pub fn refused_again(asked: Option<Duration>, waiting: Duration) -> String {
    match asked {
        Some(stated) => format!(
            "The ADS-B source asked us to wait {} again; waiting {}.",
            humanised(stated),
            humanised(waiting),
        ),
        None => format!(
            "The ADS-B source refused another request (429) without naming a delay; waiting {}.",
            humanised(waiting),
        ),
    }
}

/// The five-minute reminder while a run lasts.
///
/// It names the interval in use because that is the question the count raises:
/// "and what is it doing about it?".
#[must_use]
pub fn still_refusing(count: u64, over: Duration, every: Duration) -> String {
    format!(
        "The ADS-B source has rate-limited us (429) {count} times in the last {}; polling every \
         {}.",
        humanised(over),
        humanised(every),
    )
}

/// The end of a run.
#[must_use]
pub fn stopped_refusing(count: u64, over: Duration) -> String {
    format!(
        "The ADS-B source has stopped rate-limiting us; {count} requests were refused (429) over \
         {}.",
        humanised(over),
    )
}

/// The one line an interval change is worth.
///
/// `floor` is as far back down as a clean run will ever bring it, which is what
/// an operator who has just read "every 23s" wants to know next.
#[must_use]
pub fn changed(name: &str, change: Change, floor: Duration) -> String {
    match change {
        Change::Stated(interval) => format!(
            "{name} asks for {} between requests; polling at that rate from now on.",
            humanised(interval),
        ),
        Change::Raised(interval) => format!(
            "Polling {name} every {} after repeated rate limits (429) that named no delay; this \
             eases back towards {} after {CLEAN_RUN} clean polls.",
            humanised(interval),
            humanised(floor),
        ),
        Change::Eased { interval, after } => format!(
            "Back to polling {name} every {} after {after} clean polls.",
            humanised(interval),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn seconds(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_delay_the_provider_stated_is_the_only_thing_it_is_said_to_have_asked_for() {
        assert_eq!(
            refused(Some(seconds(10)), seconds(10)),
            "The ADS-B source asked us to wait 10s before the next request.",
        );
    }

    #[test]
    fn a_refusal_that_named_no_delay_never_puts_our_guess_in_the_providers_mouth() {
        // The Dublin finding: `seconds=20` at `poll = "10s"` and `seconds=10` at
        // `poll = "5s"` — twice the interval both times, which is our fallback.
        for said in [refused(None, seconds(20)), refused_again(None, seconds(20))] {
            assert!(!said.contains("asked"), "{said}");
            assert!(said.contains("(429) without naming a delay"), "{said}");
            assert!(said.contains("waiting 20s"), "{said}");
        }

        assert_eq!(
            refused(None, seconds(20)),
            "The ADS-B source refused a request (429) without naming a delay; waiting 20s before \
             the next one.",
        );
    }

    #[test]
    fn a_stated_delay_we_are_not_waiting_exactly_says_both_numbers() {
        // `Retry-After: 1` under a ten-second poll, and a `Retry-After` of a
        // day under the five-minute cap: what it asked, and what we are doing.
        assert_eq!(
            refused(Some(seconds(1)), seconds(10)),
            "The ADS-B source asked us to wait 1s before the next request; waiting 10s.",
        );
        assert_eq!(
            refused(Some(seconds(86_400)), seconds(300)),
            "The ADS-B source asked us to wait 24h00m before the next request; waiting 5m00s.",
        );
    }

    #[test]
    fn the_reminder_and_the_recovery_attribute_nothing_to_anybody() {
        let reminder = still_refusing(23, seconds(300), seconds(15));
        let recovery = stopped_refusing(31, seconds(912));

        assert_eq!(
            reminder,
            "The ADS-B source has rate-limited us (429) 23 times in the last 5m00s; polling every \
             15s.",
        );
        assert_eq!(
            recovery,
            "The ADS-B source has stopped rate-limiting us; 31 requests were refused (429) over \
             15m12s.",
        );
        assert!(!reminder.contains("asked") && !recovery.contains("asked"));
    }

    #[test]
    fn each_kind_of_interval_change_is_one_sentence_that_says_why() {
        assert_eq!(
            changed("adsb.lol", Change::Stated(seconds(10)), seconds(10)),
            "adsb.lol asks for 10s between requests; polling at that rate from now on.",
        );
        assert_eq!(
            changed("adsb.lol", Change::Raised(seconds(15)), seconds(10)),
            "Polling adsb.lol every 15s after repeated rate limits (429) that named no delay; \
             this eases back towards 10s after 60 clean polls.",
        );
        assert_eq!(
            changed(
                "adsb.lol",
                Change::Eased {
                    interval: seconds(10),
                    after: 60,
                },
                seconds(10),
            ),
            "Back to polling adsb.lol every 10s after 60 clean polls.",
        );
    }
}
