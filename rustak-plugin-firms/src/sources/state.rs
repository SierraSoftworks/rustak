//! What the upstream is doing, and when it is worth asking again.
//!
//! The machine is [`rustak_client::feed::upstream::SourceState`], which every
//! polled feed plugin shares: a floor under how often it reaches out that is
//! independent of the sidecar's tick, a capped backoff, `Retry-After` honoured
//! above `poll`, and each run — FIRMS not answering, FIRMS saying "not so
//! fast" — said once, reminded about every five minutes at most, and closed
//! with one line. What is FIRMS' own is here: its ceilings, that a refusal is
//! an answer, and its words.
//!
//! # `poll` is a floor, not a pin
//!
//! The interval is the fastest FIRMS is asked. A `Retry-After` it states is
//! honoured above it, up to [`MAX_BACKOFF`]; a `429` that states none is waited
//! out on twice the interval, and said to be our own guess.

use std::time::Duration;

use rustak_client::feed::upstream::{self, Rules, humanised};

/// The longest the source waits between attempts, however many have failed,
/// and the longest a stated `Retry-After` is believed.
///
/// An hour: satellites pass a few times a day, so a feed that has been down
/// all night loses nothing by waking up within the hour; and FIRMS' quota is a
/// ten-minute window, so a longer wait is a mistake somewhere, and a mistake
/// must not silence a fire feed for a day.
pub const MAX_BACKOFF: Duration = Duration::from_secs(3_600);

/// How NASA FIRMS is doing.
pub type SourceState = upstream::SourceState<Firms>;

/// FIRMS' rules, and what the FIRMS source says.
#[derive(Clone, Copy, Debug)]
pub struct Firms;

impl Rules for Firms {
    const MAX_BACKOFF: Duration = MAX_BACKOFF;

    /// An upstream that is rate limiting is reachable and working, so a source
    /// whose very first reply is a `429` is not reported as one that has never
    /// worked, which would send an administrator looking for a wrong setting
    /// that is not there.
    const REFUSAL_IS_AN_ANSWER: bool = true;

    /// The run of `429`s settles over three polls rather than ending on the
    /// next answer, so a key refused every other poll is one run.
    const REFUSALS_SETTLE_OVER_POLLS: u32 = 3;

    fn subject(_name: &str) -> String {
        "The FIRMS source".to_string()
    }

    // Said as what happened: a wait FIRMS named is FIRMS', and one it did not
    // name is our own guess and must not be attributed to it.
    fn refused(_name: &str, asked: Option<Duration>, waiting: Duration) -> String {
        let seconds = waiting.as_secs();

        if asked.is_some() {
            format!("The FIRMS source asked us to wait {seconds}s before the next request.")
        } else {
            format!(
                "The FIRMS source refused a request (429) without naming a delay; waiting \
                 {seconds}s."
            )
        }
    }

    fn refused_again(_name: &str, _asked: Option<Duration>, waiting: Duration) -> String {
        format!(
            "The FIRMS source refused a request again; waiting {}s.",
            waiting.as_secs()
        )
    }

    fn still_refusing(
        _name: &str,
        count: u64,
        over: Duration,
        _every: Duration,
        waiting: Duration,
    ) -> String {
        format!(
            "The FIRMS source is still refusing requests: {count} in the last {}; waiting {}s.",
            humanised(over),
            waiting.as_secs(),
        )
    }

    fn stopped_refusing(_name: &str, count: u64, over: Duration) -> String {
        format!(
            "The FIRMS source has stopped refusing requests; it refused {count} over {}.",
            humanised(over),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use rustak_client::feed::upstream::Report;

    const TEN_MINUTES: Duration = Duration::from_secs(600);

    fn now() -> DateTime<Utc> {
        "2026-09-22T14:00:00Z".parse().expect("an instant")
    }

    fn after(minutes: i64) -> DateTime<Utc> {
        now() + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn a_source_is_asked_once_an_interval_whatever_the_tick_is() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());

        assert!(state.ready_at(now()), "a new source may be asked at once");

        state.succeeded_at(now());

        assert!(!state.ready_at(after(9)));
        assert!(state.ready_at(after(10)));
        assert!(state.is_connected() && state.ever_connected());
    }

    #[test]
    fn failures_back_off_to_a_ceiling_and_a_success_resets_them() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        let mut waits = Vec::new();

        for _ in 0..5 {
            state.failed_at("timed out", now());
            waits.push((state.next_attempt() - now()).num_minutes());
        }

        assert_eq!(waits, [10, 20, 40, 60, 60]);
        assert!(!state.ever_connected(), "it has never worked");
        assert_eq!(state.last_error(), Some("timed out"));

        state.succeeded_at(now());

        assert_eq!(state.last_error(), None);
        assert_eq!((state.next_attempt() - now()).num_minutes(), 10);
    }

    #[test]
    fn a_rate_limit_is_an_answer_even_when_it_is_the_first_one() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        state.failed_at("timed out", now());

        state.wait_for_at(None, now());

        assert!(state.is_connected() && state.ever_connected());
        assert_eq!(state.last_error(), None);
    }

    #[test]
    fn being_asked_to_wait_is_not_an_outage() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        state.succeeded_at(now());

        state.wait_for_at(Some(Duration::from_secs(1_800)), now());

        assert!(state.is_connected());
        assert_eq!(state.rate_limited(), 1);
        assert!(!state.ready_at(after(29)));
        assert!(state.ready_at(after(30)));

        // A delay shorter than the interval never speeds the source up.
        state.wait_for_at(Some(Duration::from_secs(5)), now());

        assert!(!state.ready_at(after(9)));
    }

    #[test]
    fn an_explicit_poll_is_a_floor_a_stated_retry_after_raises() {
        // `poll = "1m"`, and FIRMS says fifteen minutes: fifteen it is, and the
        // operator's minute is the interval again once FIRMS answers.
        let mut state = SourceState::new_at("FIRMS", Duration::from_secs(60), now());

        state.wait_for_at(Some(Duration::from_secs(900)), now());

        assert!(!state.ready_at(after(14)), "not at the operator's minute");
        assert!(state.ready_at(after(15)));

        state.succeeded_at(after(15));

        assert!(!state.ready_at(after(15)));
        assert!(state.ready_at(after(16)), "the floor is where it was");
    }

    #[test]
    fn a_refusal_is_said_once_when_it_starts_and_once_when_it_is_over() {
        let mut state = SourceState::new_at("FIRMS", TEN_MINUTES, now());
        state.succeeded_at(now());

        assert_eq!(state.wait_for_at(None, after(10)).1, Report::First);

        // Answered twenty minutes later, as asked: the run has not settled,
        // so a key refused every other poll is one run, not a line a poll.
        assert_eq!(state.succeeded_at(after(30)).1, Report::Quiet);

        assert!(matches!(
            state.succeeded_at(after(40)).1,
            Report::Recovered { count: 1, .. }
        ));
        assert_eq!(state.succeeded_at(after(50)).1, Report::Quiet, "once");
    }

    #[test]
    fn the_source_is_the_firms_source_in_every_line_and_names_its_own_waits() {
        let seconds = Duration::from_secs;

        assert_eq!(
            Firms::refused("NASA FIRMS", Some(seconds(900)), seconds(900)),
            "The FIRMS source asked us to wait 900s before the next request.",
        );
        assert_eq!(
            Firms::refused("NASA FIRMS", None, seconds(1200)),
            "The FIRMS source refused a request (429) without naming a delay; waiting 1200s.",
        );
        assert_eq!(
            Firms::still_refusing("NASA FIRMS", 3, seconds(1800), seconds(600), seconds(1200)),
            "The FIRMS source is still refusing requests: 3 in the last 30m00s; waiting 1200s.",
        );
        assert_eq!(
            Firms::stopped_refusing("NASA FIRMS", 3, seconds(3600)),
            "The FIRMS source has stopped refusing requests; it refused 3 over 1h00m.",
        );
        assert_eq!(
            Firms::answered_again("NASA FIRMS", 4, seconds(300)),
            "The FIRMS source answered again after 5m00s and 4 failed attempts; the feed is \
             connected.",
        );
        assert_eq!(
            Firms::stopped_answering("NASA FIRMS", "timed out"),
            "The FIRMS source stopped answering; retrying with backoff. timed out",
        );
    }
}
