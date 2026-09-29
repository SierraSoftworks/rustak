//! What a plugin decides about its upstream: the ceilings, and the words.
//!
//! [`SourceState`](super::SourceState) is the same machine for every polled
//! feed. What is genuinely each plugin's own is here — how long it will back off
//! for, how far it believes a `Retry-After`, whether a refusal counts as an
//! answer — and what its log lines say. Everything has a default except the
//! backoff ceiling and the sentence's subject, so a new feed states two things
//! and gets the rest.
//!
//! # The words say who chose the number
//!
//! The second live ADS-B deployment logged "asked us to wait … seconds=20"
//! against a provider that had asked for nothing: the twenty seconds was the
//! plugin's own fallback, and the line put our guess in the server's mouth. So
//! the default refusal lines only ever write "asked us to wait" about a delay
//! the response stated; a refusal that named none says so, and says the wait
//! is ours. A plugin that overrides them must keep to that.
//!
//! They are functions that answer a [`String`] rather than logging, so a suite
//! can assert the wording without capturing a log; the level and the fields a
//! line carries are [`SourceState`](super::SourceState)'s and the same for
//! every feed.

use std::fmt::Debug;
use std::time::Duration;

use super::humanised;

/// One polled upstream's rules and words.
///
/// Implemented on a unit type, which is never constructed:
///
/// ```
/// use std::time::Duration;
/// use rustak_client::feed::upstream::{Rules, SourceState};
///
/// #[derive(Clone, Copy, Debug)]
/// struct Tides;
///
/// impl Rules for Tides {
///     const MAX_BACKOFF: Duration = Duration::from_secs(900);
///
///     fn subject(_name: &str) -> String {
///         "The tide gauge".to_string()
///     }
/// }
///
/// let state = SourceState::<Tides>::new("tides.example", Duration::from_secs(60));
/// assert!(state.ready());
/// ```
pub trait Rules: Clone + Copy + Debug + Send + Sync + 'static {
    /// The longest the source waits between attempts, however many have
    /// failed. Never shorter than the interval: a `poll` longer than this is
    /// still the fastest the source asks.
    const MAX_BACKOFF: Duration;

    /// The longest a stated `Retry-After` is believed. A header that says
    /// "next week" is a misconfiguration somewhere, and not a reason to go
    /// dark. Defaults to [`MAX_BACKOFF`](Self::MAX_BACKOFF).
    const MAX_RETRY_AFTER: Duration = Self::MAX_BACKOFF;

    /// Whether a refusal (`429`) is an **answer**: it ends an outage run, and
    /// a source whose first reply was a refusal is not reported as one that
    /// has never worked. When `false` a refusal leaves the connection state
    /// alone and only moves the schedule.
    const REFUSAL_IS_AN_ANSWER: bool = false;

    /// How many polls without a refusal end a run of them, on top of
    /// [`REMIND_EVERY`](super::REMIND_EVERY): the run settles after whichever
    /// is longer, so a provider refusing every other poll is one run.
    const REFUSALS_SETTLE_OVER_POLLS: u32 = 0;

    /// Who every sentence below is about: "The FIRMS source", or the
    /// upstream's own `name`.
    fn subject(name: &str) -> String;

    /// The first answer the source has ever had.
    fn answering(name: &str) -> String {
        format!("{} answered; the feed is connected.", Self::subject(name))
    }

    /// The first answer after an outage of `count` failures over `over`.
    fn answered_again(name: &str, count: u64, over: Duration) -> String {
        format!(
            "{} answered again after {} and {count} failed attempts; the feed is connected.",
            Self::subject(name),
            humanised(over),
        )
    }

    /// The first failure of an outage.
    fn stopped_answering(name: &str, error: &str) -> String {
        format!(
            "{} stopped answering; retrying with backoff. {error}",
            Self::subject(name),
        )
    }

    /// The reminder while an outage lasts: down `down_for`, `count` attempts
    /// failed in the last `over`.
    fn not_answering(
        name: &str,
        down_for: Duration,
        count: u64,
        over: Duration,
        error: &str,
    ) -> String {
        format!(
            "{} has not answered for {}; {count} attempts failed in the last {}. {error}",
            Self::subject(name),
            humanised(down_for),
            humanised(over),
        )
    }

    /// Every failure between those, which is `debug`.
    fn still_not_answering(name: &str, error: &str) -> String {
        format!("{} is still not answering. {error}", Self::subject(name))
    }

    /// The first refusal of a run. `asked` is what the response stated, when
    /// it stated anything; `waiting` is what the source will actually wait,
    /// which is not always the same number.
    fn refused(name: &str, asked: Option<Duration>, waiting: Duration) -> String {
        let subject = Self::subject(name);

        match asked {
            Some(stated) if stated == waiting => format!(
                "{subject} asked us to wait {} before the next request.",
                humanised(stated),
            ),
            Some(stated) => format!(
                "{subject} asked us to wait {} before the next request; waiting {}.",
                humanised(stated),
                humanised(waiting),
            ),
            None => format!(
                "{subject} refused a request (429) without naming a delay; waiting {} before the \
                 next one.",
                humanised(waiting),
            ),
        }
    }

    /// Every refusal after the first, which is `debug`.
    fn refused_again(name: &str, asked: Option<Duration>, waiting: Duration) -> String {
        let subject = Self::subject(name);

        match asked {
            Some(stated) => format!(
                "{subject} asked us to wait {} again; waiting {}.",
                humanised(stated),
                humanised(waiting),
            ),
            None => format!(
                "{subject} refused another request (429) without naming a delay; waiting {}.",
                humanised(waiting),
            ),
        }
    }

    /// The reminder while a run of refusals lasts: `count` in the last `over`,
    /// polling `every`, and this one waited out for `waiting`.
    fn still_refusing(
        name: &str,
        count: u64,
        over: Duration,
        every: Duration,
        waiting: Duration,
    ) -> String {
        let _ = waiting;

        format!(
            "{} has rate-limited us (429) {count} times in the last {}; polling every {}.",
            Self::subject(name),
            humanised(over),
            humanised(every),
        )
    }

    /// The end of a run of refusals.
    fn stopped_refusing(name: &str, count: u64, over: Duration) -> String {
        format!(
            "{} has stopped rate-limiting us; {count} requests were refused (429) over {}.",
            Self::subject(name),
            humanised(over),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words every plugin gets unless it says otherwise, under the subject
    /// the ADS-B plugin gives them — which is where these assertions came from.
    #[derive(Clone, Copy, Debug)]
    struct Adsb;

    impl Rules for Adsb {
        const MAX_BACKOFF: Duration = Duration::from_secs(300);

        fn subject(_name: &str) -> String {
            "The ADS-B source".to_string()
        }
    }

    const fn seconds(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_delay_the_provider_stated_is_the_only_thing_it_is_said_to_have_asked_for() {
        assert_eq!(
            Adsb::refused("adsb.lol", Some(seconds(10)), seconds(10)),
            "The ADS-B source asked us to wait 10s before the next request.",
        );
    }

    #[test]
    fn a_refusal_that_named_no_delay_never_puts_our_guess_in_the_providers_mouth() {
        // The Dublin finding: `seconds=20` at `poll = "10s"` and `seconds=10` at
        // `poll = "5s"` — twice the interval both times, which is our fallback.
        for said in [
            Adsb::refused("adsb.lol", None, seconds(20)),
            Adsb::refused_again("adsb.lol", None, seconds(20)),
        ] {
            assert!(!said.contains("asked"), "{said}");
            assert!(said.contains("(429) without naming a delay"), "{said}");
            assert!(said.contains("waiting 20s"), "{said}");
        }

        assert_eq!(
            Adsb::refused("adsb.lol", None, seconds(20)),
            "The ADS-B source refused a request (429) without naming a delay; waiting 20s before \
             the next one.",
        );
    }

    #[test]
    fn a_stated_delay_we_are_not_waiting_exactly_says_both_numbers() {
        // `Retry-After: 1` under a ten-second poll, and a `Retry-After` of a
        // day under the five-minute cap: what it asked, and what we are doing.
        assert_eq!(
            Adsb::refused("adsb.lol", Some(seconds(1)), seconds(10)),
            "The ADS-B source asked us to wait 1s before the next request; waiting 10s.",
        );
        assert_eq!(
            Adsb::refused("adsb.lol", Some(seconds(86_400)), seconds(300)),
            "The ADS-B source asked us to wait 24h00m before the next request; waiting 5m00s.",
        );
    }

    #[test]
    fn the_reminder_and_the_recovery_attribute_nothing_to_anybody() {
        let reminder = Adsb::still_refusing("adsb.lol", 23, seconds(300), seconds(15), seconds(30));
        let recovery = Adsb::stopped_refusing("adsb.lol", 31, seconds(912));

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
    fn an_outage_is_said_in_the_words_every_feed_shares() {
        assert_eq!(
            Adsb::answering("adsb.lol"),
            "The ADS-B source answered; the feed is connected.",
        );
        assert_eq!(
            Adsb::answered_again("adsb.lol", 4, seconds(252)),
            "The ADS-B source answered again after 4m12s and 4 failed attempts; the feed is \
             connected.",
        );
        assert_eq!(
            Adsb::stopped_answering("adsb.lol", "timed out"),
            "The ADS-B source stopped answering; retrying with backoff. timed out",
        );
        assert_eq!(
            Adsb::not_answering("adsb.lol", seconds(3720), 12, seconds(300), "timed out"),
            "The ADS-B source has not answered for 1h02m; 12 attempts failed in the last 5m00s. \
             timed out",
        );
        assert_eq!(
            Adsb::still_not_answering("adsb.lol", "timed out"),
            "The ADS-B source is still not answering. timed out",
        );
    }
}
