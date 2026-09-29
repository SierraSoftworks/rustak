//! What a source says about its cadence changing, in one place.
//!
//! # Why the words are their own module
//!
//! The second live deployment logged `The ADS-B source asked us to wait before
//! the next request. seconds=20` against a provider that had asked for nothing:
//! adsb.lol answered `429` with no `Retry-After` at all, the twenty seconds was
//! this plugin's own fallback, and the line put our guess in the server's
//! mouth. So the rule is that **a sentence says who chose the number**.
//!
//! The refusal lines themselves are the shared ones from
//! [`rustak_client::feed::upstream::Rules`], which keep that rule for every
//! feed. What is left here is what only this plugin says: why its interval
//! moved. They are functions that answer a [`String`] rather than `info!`
//! calls at the point of use so that the suites can assert the distinction
//! without reading a log.

use std::time::Duration;

use rustak_client::feed::upstream::humanised;

use super::cadence::{CLEAN_RUN, Change};

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
        Change::Eased {
            interval,
            after,
            over,
        } => format!(
            "Back to polling {name} every {} after {after} clean polls ({}).",
            humanised(interval),
            humanised(over),
        ),
        Change::ProbeRefused {
            interval,
            rung,
            retry_after,
        } => format!(
            "Polling {name} every {} again: {} was refused (429). It will be tried again after \
             {retry_after} clean polls (about {}).",
            humanised(interval),
            humanised(rung),
            humanised(interval.saturating_mul(u32::try_from(retry_after).unwrap_or(u32::MAX))),
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
                    over: seconds(900),
                },
                seconds(10),
            ),
            "Back to polling adsb.lol every 10s after 60 clean polls (15m00s).",
        );
        assert_eq!(
            changed(
                "adsb.lol",
                Change::ProbeRefused {
                    interval: seconds(35),
                    rung: seconds(23),
                    retry_after: 120,
                },
                seconds(10),
            ),
            "Polling adsb.lol every 35s again: 23s was refused (429). It will be tried again \
             after 120 clean polls (about 1h10m).",
        );
    }
}
