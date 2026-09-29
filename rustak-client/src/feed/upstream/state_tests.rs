//! The shared machine, on a clock these tests move by hand.
//!
//! Three sets of rules, one per shape the feed plugins use: a guest that backs
//! off to five minutes and treats a refusal as nothing but a schedule (ADS-B),
//! one that believes a stated delay further than it backs off (ESB), and one
//! for which a refusal is an answer (FIRMS). What each plugin chose is tested
//! in that plugin; what the machine does with a choice is tested here.

use super::*;

#[derive(Clone, Copy, Debug)]
struct Guest;

impl Rules for Guest {
    const MAX_BACKOFF: Duration = Duration::from_secs(300);

    fn subject(_name: &str) -> String {
        "The guest source".to_string()
    }
}

#[derive(Clone, Copy, Debug)]
struct Patient;

impl Rules for Patient {
    const MAX_BACKOFF: Duration = Duration::from_secs(900);
    const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);
    const REFUSALS_SETTLE_OVER_POLLS: u32 = 3;

    fn subject(name: &str) -> String {
        name.to_string()
    }
}

#[derive(Clone, Copy, Debug)]
struct Answering;

impl Rules for Answering {
    const MAX_BACKOFF: Duration = Duration::from_secs(3600);
    const REFUSAL_IS_AN_ANSWER: bool = true;
    const REFUSALS_SETTLE_OVER_POLLS: u32 = 3;

    fn subject(_name: &str) -> String {
        "The answering source".to_string()
    }
}

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
}

fn every<R: Rules>(seconds: u64) -> SourceState<R> {
    SourceState::new_at("upstream", Duration::from_secs(seconds), at(0))
}

#[test]
fn a_new_source_may_be_asked_at_once_and_has_never_answered() {
    let state = every::<Guest>(5);

    assert!(state.ready_at(at(0)), "the first poll happens immediately");
    assert!(!state.is_connected());
    assert!(!state.ever_connected());
    assert_eq!(state.last_success(), None);
    assert_eq!(state.last_error(), None);
    assert_eq!(state.interval(), Duration::from_secs(5));
    assert_eq!(state.rate_limited(), 0);
}

#[test]
fn an_answer_holds_the_next_request_off_for_an_interval_whatever_the_tick_is() {
    let mut state = every::<Guest>(5);

    state.succeeded_at(at(0));

    assert!(state.is_connected());
    assert!(state.ever_connected());
    assert_eq!(state.last_success(), Some(at(0)));
    assert!(!state.ready_at(at(4)), "five seconds have not passed");
    assert!(state.ready_at(at(5)));
    assert_eq!(state.next_attempt(), at(5));
    assert_eq!(state.reconnecting_for(), None);
}

#[test]
fn the_backoff_doubles_and_then_stops_doubling() {
    let mut state = every::<Guest>(5);

    for (failures, expected) in [
        (1_u32, 5),
        (2, 10),
        (3, 20),
        (4, 40),
        (5, 80),
        (6, 160),
        (7, 300),
        (8, 300),
    ] {
        state.failed_at("timed out", at(0));

        assert_eq!(state.failures(), failures);
        assert_eq!(
            state.next_attempt() - at(0),
            chrono::Duration::seconds(expected),
            "after {failures} failures",
        );
    }

    for _ in 8..80 {
        state.failed_at("timed out", at(0));
    }

    assert_eq!(
        state.next_attempt() - at(0),
        chrono::Duration::seconds(300),
        "and the shift never overflows",
    );
}

#[test]
fn a_backoff_ceiling_below_the_interval_never_speeds_the_source_up() {
    let mut state = every::<Guest>(600);

    state.failed_at("timed out", at(0));
    state.failed_at("timed out", at(0));

    assert_eq!(state.next_attempt(), at(600));
}

#[test]
fn a_failure_remembers_what_went_wrong_and_that_it_never_worked() {
    let mut state = every::<Guest>(5);

    state.failed_at("the receiver refused the connection", at(0));

    assert!(!state.is_connected());
    assert!(!state.ever_connected(), "it has never answered");
    assert_eq!(
        state.last_error(),
        Some("the receiver refused the connection")
    );
    assert!(state.reconnecting_for().is_some());
    assert!(!state.ready_at(at(4)));
    assert!(state.in_outage());
}

#[test]
fn a_failure_after_an_answer_still_knows_it_once_worked() {
    let mut state = every::<Patient>(300);
    state.succeeded_at(at(0));

    state.failed_at("connection refused", at(300));

    assert!(!state.is_connected());
    assert_eq!(state.last_error(), Some("connection refused"));
    assert_eq!(state.last_success(), Some(at(0)), "it did work once");
    assert!(state.ever_connected());
    assert!(!state.ready_at(at(599)));
}

#[test]
fn recovering_clears_the_failure_and_the_backoff() {
    let mut state = every::<Guest>(5);

    state.failed_at("timed out", at(0));
    state.failed_at("timed out", at(10));
    state.succeeded_at(at(30));

    assert_eq!(state.failures(), 0);
    assert_eq!(state.last_error(), None);
    assert!(state.is_connected());
    assert!(!state.in_outage(), "and the run is over");
    assert_eq!(state.next_attempt(), at(35));
}

#[test]
fn an_outage_is_said_once_when_it_starts_and_once_when_it_ends() {
    let mut state = every::<Patient>(60);
    state.succeeded_at(at(0));

    let said: Vec<Report> = (1..=4)
        .map(|minute| state.failed_at("timed out", at(minute * 60)))
        .collect();

    assert_eq!(said[0], Report::First);
    assert!(
        said[1..].iter().all(|report| *report == Report::Quiet),
        "{said:?}"
    );
    assert!(matches!(
        state.succeeded_at(at(290)).0,
        Report::Recovered { count: 4, .. }
    ));
    assert_eq!(
        state.succeeded_at(at(590)).0,
        Report::Quiet,
        "and only once"
    );
}

#[test]
fn an_outage_that_lasts_is_reminded_about_with_a_count() {
    let mut state = every::<Guest>(60);

    assert_eq!(state.failed_at("timed out", at(0)), Report::First);

    for minute in 1..5 {
        assert_eq!(state.failed_at("timed out", at(minute * 60)), Report::Quiet);
    }

    assert_eq!(
        state.failed_at("timed out", at(300)),
        Report::Reminder {
            count: 5,
            over: Duration::from_secs(300),
        },
    );
}

#[test]
fn a_rate_limit_moves_the_schedule_without_marking_a_failure() {
    let mut state = every::<Guest>(5);
    state.succeeded_at(at(0));

    let (wait, _) = state.wait_for_at(Some(Duration::from_secs(60)), at(5));

    assert_eq!(wait, Duration::from_secs(60));
    assert!(state.is_connected(), "429 is an upstream that is working");
    assert_eq!(state.last_error(), None);
    assert_eq!(state.rate_limited(), 1);
    assert!(!state.ready_at(at(64)));
    assert!(state.ready_at(at(65)));
}

#[test]
fn a_refusal_that_is_not_an_answer_leaves_the_connection_state_alone() {
    let mut state = every::<Guest>(5);
    state.failed_at("timed out", at(0));

    state.wait_for_at(None, at(5));

    assert!(!state.is_connected());
    assert!(!state.ever_connected());
    assert!(state.in_outage(), "the outage is still going");
    assert_eq!(state.last_error(), Some("timed out"));
}

#[test]
fn a_refusal_that_is_an_answer_ends_an_outage_even_when_it_is_the_first_reply() {
    let mut state = every::<Answering>(600);
    state.failed_at("timed out", at(0));

    state.wait_for_at(None, at(600));

    assert!(state.is_connected() && state.ever_connected());
    assert!(!state.in_outage());
    assert_eq!(state.last_error(), None);
    assert_eq!(state.last_success(), None, "a refusal is not a success");
    assert_eq!(state.since(), at(600));
}

#[test]
fn a_rate_limit_never_asks_us_to_wait_longer_than_the_cap() {
    let mut state = every::<Guest>(5);

    let (wait, _) = state.wait_for_at(Some(Duration::from_secs(86_400)), at(0));

    // A `Retry-After` of a day is either a mistake or a ban; either way a
    // sidecar that stopped polling until tomorrow would never notice it
    // being lifted.
    assert_eq!(wait, Guest::MAX_BACKOFF);
    assert!(state.ready_at(at(300)));
}

#[test]
fn a_stated_delay_is_believed_further_than_the_backoff_when_the_rules_say_so() {
    let mut state = every::<Patient>(300);

    let (stated, _) = state.wait_for_at(Some(Duration::from_secs(2400)), at(0));
    let (guessed, _) = every::<Patient>(600).wait_for_at(None, at(0));

    assert_eq!(stated, Duration::from_secs(2400), "above MAX_BACKOFF");
    assert_eq!(guessed, Duration::from_secs(900), "our guess stops there");
}

#[test]
fn a_short_rate_limit_still_respects_the_configured_interval() {
    let mut state = every::<Guest>(5);

    let (wait, _) = state.wait_for_at(Some(Duration::from_millis(100)), at(0));

    assert_eq!(wait, Duration::from_secs(5));
    assert!(!state.ready_at(at(4)));
}

#[test]
fn a_refusal_that_names_no_delay_waits_twice_the_interval() {
    let mut state = every::<Guest>(5);

    let (wait, _) = state.wait_for_at(None, at(0));

    assert_eq!(wait, Duration::from_secs(10));
    assert!(!state.ready_at(at(9)));
    assert!(state.ready_at(at(10)));
}

#[test]
fn an_explicit_poll_is_a_floor_a_stated_retry_after_raises() {
    // `poll = "1m"`, and the upstream says ten minutes: ten minutes it is, and
    // the interval is back to the operator's once it answers again.
    let mut state = every::<Patient>(60);

    let (wait, _) = state.wait_for_at(Some(Duration::from_secs(600)), at(0));

    assert_eq!(wait, Duration::from_secs(600));
    assert!(!state.ready_at(at(599)), "not at the operator's minute");
    assert!(state.ready_at(at(600)));

    let (wait, _) = state.wait_for_at(Some(Duration::from_secs(5)), at(600));

    assert_eq!(wait, Duration::from_secs(60), "and never below it");

    state.succeeded_at(at(660));

    assert!(!state.ready_at(at(719)));
    assert!(state.ready_at(at(720)), "the floor is where it was");
}

#[test]
fn a_moved_interval_is_what_the_next_schedule_is_kept_against() {
    // The seam an adaptive cadence (ADS-B's) moves the floor through.
    let mut state = every::<Guest>(10);

    state.set_interval(Duration::from_secs(23));
    state.succeeded_at(at(0));

    assert_eq!(state.next_attempt(), at(23));

    let (wait, _) = state.wait_for_at(None, at(23));

    assert_eq!(wait, Duration::from_secs(46), "twice the interval in use");
}

#[test]
fn a_refusal_is_said_once_when_it_starts_and_once_when_it_is_over() {
    let mut state = every::<Patient>(300);
    state.succeeded_at(at(0));

    assert_eq!(state.wait_for_at(None, at(300)).1, Report::First);
    assert_eq!(state.wait_for_at(None, at(360)).1, Report::Quiet);

    // Answered, but the run has not settled over three polls: a provider
    // refusing every other request is one run, not one line per refusal.
    assert_eq!(state.succeeded_at(at(960)).1, Report::Quiet);
    assert!(state.being_refused());
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
    assert!(!state.being_refused());
    assert_eq!(state.succeeded_at(at(3000)).1, Report::Quiet, "once");
}

#[test]
fn a_run_of_refusals_settles_after_five_minutes_when_no_polls_are_asked_for() {
    // A provider that refuses every other request is one run of notices.
    let mut state = every::<Guest>(5);

    state.wait_for_at(Some(Duration::from_secs(10)), at(0));

    for poll in 1..24 {
        state.succeeded_at(at(poll * 10));
        state.wait_for_at(Some(Duration::from_secs(10)), at(poll * 10 + 5));
    }

    assert!(
        state.being_refused(),
        "a success in between does not end a run of rate limiting",
    );

    state.succeeded_at(at(1_000));

    assert!(
        !state.being_refused(),
        "five minutes without one is the end"
    );
}

#[test]
fn due_at_makes_the_next_request_due_without_touching_anything_else() {
    let mut state = every::<Guest>(5);
    state.failed_at("timed out", at(0));
    state.failed_at("timed out", at(5));

    state.due_at(at(6));

    assert!(state.ready_at(at(6)));
    assert_eq!(state.failures(), 2);

    state.failed_at("timed out", at(6));

    assert_eq!(state.next_attempt(), at(26), "the backoff carries on");
}
