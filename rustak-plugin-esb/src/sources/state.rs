//! What an upstream is doing, and when it is worth asking again.
//!
//! The machine is [`rustak_client::feed::upstream::SourceState`], which every
//! polled feed plugin shares: a floor under the request rate that is
//! independent of the sidecar's tick, a capped backoff, enough memory to tell
//! an administrator whether the feed is fine, struggling, or has never worked,
//! and each run — ESB not answering, ESB saying "not so fast" — said once,
//! reminded about every five minutes, and closed with one line. What is ESB's
//! own is here: its ceilings and its words.
//!
//! # `poll` is a floor, not a pin
//!
//! The interval is the fastest this source asks. A `Retry-After` that ESB
//! states is honoured above it, up to [`MAX_RETRY_AFTER`]; one it does not
//! state is waited out on twice the interval, and said to be our own guess.

use std::time::Duration;

use rustak_client::feed::upstream::{self, Rules, humanised};

/// The longest a source waits between attempts, however many have failed.
pub const MAX_BACKOFF: Duration = Duration::from_secs(900);

/// The longest a stated `Retry-After` is believed. An upstream is taken at its
/// word well past [`MAX_BACKOFF`], but a header that says "next week" is a
/// misconfiguration somewhere, and not a reason to go dark through a storm.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// How an upstream is doing.
pub type SourceState = upstream::SourceState<Esb>;

/// ESB's rules, and what an ESB source says — under its own name, which is
/// `ESB PowerCheck` for the live source.
///
/// A `429` is **not** an answer here: it leaves the connection state alone,
/// and only the schedule moves.
#[derive(Clone, Copy, Debug)]
pub struct Esb;

impl Rules for Esb {
    const MAX_BACKOFF: Duration = MAX_BACKOFF;
    const MAX_RETRY_AFTER: Duration = MAX_RETRY_AFTER;

    /// The run of `429`s settles over three polls rather than ending on the
    /// next answer, so a provider refusing every other request is one run.
    const REFUSALS_SETTLE_OVER_POLLS: u32 = 3;

    fn subject(name: &str) -> String {
        name.to_string()
    }

    fn answering(name: &str) -> String {
        format!("{name} is answering.")
    }

    fn answered_again(name: &str, count: u64, over: Duration) -> String {
        format!(
            "{name} answered again after {} and {count} failed attempts.",
            humanised(over)
        )
    }

    // Said as what happened: a wait ESB named is ESB's, and one it did not
    // name is our own guess and must not be attributed to it.
    fn refused(name: &str, asked: Option<Duration>, waiting: Duration) -> String {
        let seconds = waiting.as_secs();

        if asked.is_some() {
            format!("{name} asked us to wait {seconds}s.")
        } else {
            format!("{name} refused a request (429) without naming a delay; waiting {seconds}s.")
        }
    }

    fn refused_again(name: &str, _asked: Option<Duration>, waiting: Duration) -> String {
        format!(
            "{name} refused a request again; waiting {}s.",
            waiting.as_secs()
        )
    }

    fn still_refusing(
        name: &str,
        count: u64,
        over: Duration,
        _every: Duration,
        waiting: Duration,
    ) -> String {
        format!(
            "{name} is still refusing requests: {count} in the last {}; waiting {}s.",
            humanised(over),
            waiting.as_secs(),
        )
    }

    fn stopped_refusing(name: &str, count: u64, over: Duration) -> String {
        format!(
            "{name} has stopped refusing requests; it refused {count} over {}.",
            humanised(over),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use rustak_client::feed::upstream::Report;

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    fn state() -> SourceState {
        SourceState::new_at("PowerCheck", Duration::from_secs(300), at(0))
    }

    #[test]
    fn a_stated_delay_is_believed_only_so_far() {
        let week = Duration::from_secs(7 * 24 * 3600);

        assert_eq!(state().wait_for_at(Some(week), at(0)).0, MAX_RETRY_AFTER);
    }

    #[test]
    fn being_asked_to_wait_is_not_a_failure() {
        let mut state = state();
        state.succeeded_at(at(0));

        let (wait, _) = state.wait_for_at(Some(Duration::from_secs(600)), at(300));

        assert_eq!(wait, Duration::from_secs(600), "longer than any backoff");
        assert!(state.is_connected());
        assert_eq!(state.last_error(), None);
        assert!(!state.ready_at(at(899)));
    }

    #[test]
    fn being_asked_to_wait_before_the_first_answer_is_not_an_answer() {
        let mut state = state();

        state.wait_for_at(None, at(0));

        assert!(!state.is_connected());
        assert_eq!(state.last_success(), None, "still never connected");
    }

    #[test]
    fn an_explicit_poll_is_a_floor_a_stated_retry_after_raises() {
        // `poll = "1m"`, and ESB says ten minutes: ten minutes it is, and the
        // interval is back to the operator's one once ESB answers again.
        let mut state = SourceState::new_at("PowerCheck", Duration::from_secs(60), at(0));

        let (wait, _) = state.wait_for_at(Some(Duration::from_secs(600)), at(0));

        assert_eq!(wait, Duration::from_secs(600));
        assert!(!state.ready_at(at(599)), "not at the operator's minute");
        assert!(state.ready_at(at(600)));

        let (wait, _) = state.wait_for_at(Some(Duration::from_secs(5)), at(600));

        assert_eq!(wait, Duration::from_secs(60), "and never below it");

        state.succeeded_at(at(660));

        assert!(state.ready_at(at(720)), "the floor is where it was");
    }

    #[test]
    fn a_refusal_is_said_once_when_it_starts_and_once_when_it_is_over() {
        let mut state = state();
        state.succeeded_at(at(0));

        assert_eq!(state.wait_for_at(None, at(300)).1, Report::First);
        assert_eq!(state.wait_for_at(None, at(360)).1, Report::Quiet);

        // Answered, but the run has not settled: a provider refusing every
        // other request is one run, not one line per refusal.
        assert_eq!(state.succeeded_at(at(960)).1, Report::Quiet);
        assert!(
            matches!(
                state.wait_for_at(None, at(1260)).1,
                Report::Reminder { count: 2, .. }
            ),
            "a reminder, with a count, and not a new run",
        );

        assert!(matches!(
            state.succeeded_at(at(1260 + 900)).1,
            Report::Recovered { count: 3, .. }
        ));
        assert_eq!(state.succeeded_at(at(3000)).1, Report::Quiet, "once");
    }

    #[test]
    fn every_line_is_about_the_source_by_name_and_names_its_own_waits() {
        let seconds = Duration::from_secs;

        assert_eq!(
            Esb::answering("ESB PowerCheck"),
            "ESB PowerCheck is answering."
        );
        assert_eq!(
            Esb::answered_again("ESB PowerCheck", 4, seconds(252)),
            "ESB PowerCheck answered again after 4m12s and 4 failed attempts.",
        );
        assert_eq!(
            Esb::stopped_answering("ESB PowerCheck", "timed out"),
            "ESB PowerCheck stopped answering; retrying with backoff. timed out",
        );
        assert_eq!(
            Esb::refused("ESB PowerCheck", Some(seconds(600)), seconds(600)),
            "ESB PowerCheck asked us to wait 600s.",
        );
        assert_eq!(
            Esb::refused("ESB PowerCheck", None, seconds(600)),
            "ESB PowerCheck refused a request (429) without naming a delay; waiting 600s.",
        );
        assert_eq!(
            Esb::still_refusing(
                "ESB PowerCheck",
                2,
                seconds(960),
                seconds(300),
                seconds(600)
            ),
            "ESB PowerCheck is still refusing requests: 2 in the last 16m00s; waiting 600s.",
        );
        assert_eq!(
            Esb::refused_again("ESB PowerCheck", None, seconds(600)),
            "ESB PowerCheck refused a request again; waiting 600s.",
        );
        assert_eq!(
            Esb::stopped_refusing("ESB PowerCheck", 3, seconds(1860)),
            "ESB PowerCheck has stopped refusing requests; it refused 3 over 31m00s.",
        );
    }
}
