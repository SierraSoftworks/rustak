//! What an upstream is doing, and when it is worth asking again.
//!
//! Every live source in this plugin is an HTTP GET on a timer, so all three
//! want the same three things: a floor under how often they reach out, a capped
//! backoff when the answer is a failure, and enough memory of what happened to
//! tell an administrator whether the feed is fine, struggling or has never
//! worked at all. That is this type, and it is deliberately the only thing in
//! the crate that logs a *state change* — a source that logged every failed
//! poll would fill a log with the same line every five seconds.
//!
//! Each of those runs — the outage, the rate limiting — is a [`Repeated`], so
//! it costs one line at the start, one every five minutes while it lasts and
//! one at the end, however many polls it spans.
//!
//! # The cadence adapts to the provider
//!
//! The interval a source was configured with is where it **starts** and the
//! fastest it will ever ask, not a promise about how often it asks. What moves
//! it is in `cadence`, and there are two rules, because there are two kinds of
//! refusal:
//!
//! - a provider that **states** a delay twice inside [`LIMIT_WINDOW`] polls is
//!   taken at its word: the interval becomes what it asked for, and nothing
//!   lowers it again while the process lives;
//! - a provider that refuses twice inside the window and **names no delay** —
//!   which is all adsb.lol has ever been seen to do — is backed off from by half
//!   as much again, up to [`MAX_ADAPTED`], and [`CLEAN_RUN`] polls in a row
//!   that nobody refused earn one step back down.
//!
//! The second rule is reversible because the number in it is our own guess,
//! and a guess made permanent is how a feed slows itself to a crawl over a week
//! of flaky minutes.
//!
//! # And it says who chose the number
//!
//! Everything this type says about a refusal comes from `wording`, which never
//! writes "asked us to wait" about a delay the provider did not state.

mod cadence;
mod wording;

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use self::cadence::{Cadence, Change};
use super::notice::{REMIND_EVERY, Repeated, Report, humanised};

pub use self::cadence::{CLEAN_RUN, LIMIT_WINDOW, MAX_ADAPTED, RECENT_POLLS};

/// The longest a source waits between attempts, however many have failed.
///
/// Five minutes: long enough that a receiver that has been unplugged for the
/// weekend is not a request every second, short enough that plugging it back in
/// puts aircraft on the map while somebody is still standing next to it.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How many doublings the backoff is allowed, before [`MAX_BACKOFF`] catches
/// it anyway. Six, so the shift cannot overflow on a source that has been
/// failing for a week.
const MAX_DOUBLINGS: u32 = 6;

/// How an upstream is doing.
#[derive(Clone, Debug)]
pub struct SourceState {
    name: String,

    /// How often it may ask: what was configured, and what refusals have since
    /// made of it.
    cadence: Cadence,
    connected: bool,
    ever_connected: bool,
    failures: u32,
    since: DateTime<Utc>,
    last_error: Option<String>,
    next_attempt: DateTime<Utc>,

    /// The run of failures, so an outage is announced once.
    outage: Repeated,

    /// The run of rate limits. It settles rather than ending on the next
    /// request that works, because a provider refusing every other request is
    /// one run and not a hundred.
    limit: Repeated,

    /// How many `429`s this source has been sent, for the heartbeat.
    rate_limited: u64,
}

impl SourceState {
    /// A source that has not been asked anything yet, and may be asked now.
    #[must_use]
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self {
            name: name.into(),
            cadence: Cadence::new(interval),
            connected: false,
            ever_connected: false,
            failures: 0,
            since: Utc::now(),
            last_error: None,
            next_attempt: Utc::now(),
            outage: Repeated::new(Duration::ZERO),
            limit: Repeated::new(REMIND_EVERY),
            rate_limited: 0,
        }
    }

    /// The upstream's name, for a log line or a heartbeat.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often this source may reach its upstream **as it stands**: the
    /// configured interval, or whatever a provider's refusals have since made
    /// of it. Never less than [`configured`](Self::configured).
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.cadence.effective()
    }

    /// The interval this source was opened with — an operator's `poll`, or the
    /// provider's own default — which is the floor under
    /// [`interval`](Self::interval) and not a pin on it.
    #[must_use]
    pub const fn configured(&self) -> Duration {
        self.cadence.configured()
    }

    /// How many of how many recent polls were refused, when that is more than
    /// half of the last [`RECENT_POLLS`]; [`None`] otherwise.
    ///
    /// Rate limiting this source absorbs by slowing down is not a degraded
    /// feed. Being refused more often than answered is.
    #[must_use]
    pub const fn mostly_refused(&self) -> Option<(u32, u32)> {
        self.cadence.mostly_refused()
    }

    /// How many times this source has been rate-limited, for the heartbeat.
    #[must_use]
    pub const fn rate_limited(&self) -> u64 {
        self.rate_limited
    }

    /// Whether it is time to reach out again.
    ///
    /// A source polls on the sidecar's tick, which may be far more often than
    /// its upstream wants to be asked; this is what makes the two independent.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.ready_at(Utc::now())
    }

    /// [`ready`](Self::ready), at an instant of the caller's choosing.
    #[must_use]
    pub fn ready_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.next_attempt
    }

    /// Records an answer, and schedules the next attempt one interval away.
    pub fn succeeded(&mut self) {
        self.succeeded_at(Utc::now());
    }

    /// [`succeeded`](Self::succeeded), at an instant of the caller's choosing.
    pub fn succeeded_at(&mut self, now: DateTime<Utc>) {
        if let Some(change) = self.cadence.succeeded() {
            self.announce(change);
        }

        match self.outage.cleared(now) {
            Report::Recovered { count, over } => {
                info!(
                    source = %self.name,
                    "The ADS-B source answered again after {} and {count} failed attempts; \
                     the feed is connected.",
                    humanised(over),
                );
                self.since = now;
            }
            _ if !self.ever_connected => {
                info!(
                    source = %self.name,
                    "The ADS-B source answered; the feed is connected.",
                );
                self.since = now;
            }
            _ => {}
        }

        if let Report::Recovered { count, over } = self.limit.cleared(now) {
            info!(source = %self.name, "{}", wording::stopped_refusing(count, over));
        }

        self.connected = true;
        self.ever_connected = true;
        self.failures = 0;
        self.last_error = None;
        self.next_attempt = now + self.interval();
    }

    /// Records a failure, and backs off.
    ///
    /// `error` is already a message fit for an administrator to read: nothing
    /// here redacts, because nothing here should ever be handed a credential.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.failed_at(error, Utc::now());
    }

    /// [`failed`](Self::failed), at an instant of the caller's choosing.
    pub fn failed_at(&mut self, error: impl Into<String>, now: DateTime<Utc>) {
        let error = error.into();
        self.cadence.failed();

        match self.outage.happened(now) {
            Report::First => {
                self.since = now;
                warn!(
                    source = %self.name,
                    "The ADS-B source stopped answering; retrying with backoff. {error}",
                );
            }
            Report::Reminder { count, over } => warn!(
                source = %self.name,
                "The ADS-B source has not answered for {}; {count} attempts failed in the last \
                 {}. {error}",
                humanised(elapsed(self.since, now)),
                humanised(over),
            ),
            _ => debug!(
                source = %self.name,
                "The ADS-B source is still not answering. {error}",
            ),
        }

        self.connected = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(error);
        self.next_attempt = now + self.backoff();
    }

    /// Holds off because the upstream said "not so fast".
    ///
    /// `asked` is the delay the response stated, when it stated one. Not a
    /// failure: an upstream saying "not so fast" is one that is working, so the
    /// connection state is left alone and only the schedule moves.
    pub fn wait_for(&mut self, asked: Option<Duration>) {
        self.wait_for_at(asked, Utc::now());
    }

    /// [`wait_for`](Self::wait_for), at an instant of the caller's choosing.
    pub fn wait_for_at(&mut self, asked: Option<Duration>, now: DateTime<Utc>) {
        self.rate_limited = self.rate_limited.saturating_add(1);

        // The cadence moves first, so that the wait below — and the line that
        // names it — are about the interval that is now in use.
        if let Some(change) = self
            .cadence
            .refused(asked.map(|stated| stated.min(MAX_BACKOFF)))
        {
            self.announce(change);
        }

        // A `429` that names no delay is waited out on twice the interval.
        // That is our own guess, and `wording` says so.
        let every = self.interval();
        let waiting = asked
            .unwrap_or_else(|| every.saturating_mul(2))
            .min(MAX_BACKOFF)
            .max(every);

        match self.limit.happened(now) {
            Report::First => info!(
                source = %self.name,
                seconds = waiting.as_secs(),
                stated = asked.is_some(),
                "{}",
                wording::refused(asked, waiting),
            ),
            Report::Reminder { count, over } => info!(
                source = %self.name,
                "{}",
                wording::still_refusing(count, over, every),
            ),
            _ => debug!(
                source = %self.name,
                seconds = waiting.as_secs(),
                stated = asked.is_some(),
                "{}",
                wording::refused_again(asked, waiting),
            ),
        }

        self.next_attempt = now + waiting;
    }

    /// Says, once, that the interval is not what it was.
    fn announce(&self, change: Change) {
        info!(
            source = %self.name,
            seconds = change.interval().as_secs(),
            "{}",
            wording::changed(&self.name, change, self.cadence.floor()),
        );
    }

    /// Whether the last attempt worked.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Whether any attempt has ever worked.
    ///
    /// The difference between "this feed is having a bad afternoon" and "this
    /// feed has never worked", which is a configuration error rather than an
    /// outage — and the difference between `degraded` and `unhealthy`.
    #[must_use]
    pub const fn ever_connected(&self) -> bool {
        self.ever_connected
    }

    /// When the current condition began.
    #[must_use]
    pub const fn since(&self) -> DateTime<Utc> {
        self.since
    }

    /// How long it has been failing, or [`None`] while it is working.
    #[must_use]
    pub fn reconnecting_for(&self) -> Option<chrono::Duration> {
        (!self.connected).then(|| Utc::now() - self.since)
    }

    /// What went wrong last, if anything has.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The wait before the next attempt: the interval, doubled once per
    /// consecutive failure, capped at [`MAX_BACKOFF`].
    fn backoff(&self) -> Duration {
        let doublings = self.failures.saturating_sub(1).min(MAX_DOUBLINGS);

        self.interval()
            .saturating_mul(1_u32 << doublings)
            .min(MAX_BACKOFF)
            .max(self.interval())
    }
}

/// `now - before`, never negative.
fn elapsed(before: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - before).to_std().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    fn state() -> SourceState {
        polling_every(5)
    }

    fn polling_every(seconds: u64) -> SourceState {
        let mut state = SourceState::new("adsb.lol", Duration::from_secs(seconds));
        state.since = at(0);
        state.next_attempt = at(0);

        state
    }

    /// A provider that answers one request every `tolerates`, refuses anything
    /// sooner, and never says how long to wait: adsb.lol as the second Dublin
    /// deployment met it, with the number nobody publishes written down.
    struct Grudging {
        tolerates: Duration,
        answered_at: Option<DateTime<Utc>>,
    }

    impl Grudging {
        fn tolerating(seconds: u64) -> Self {
            Self {
                tolerates: Duration::from_secs(seconds),
                answered_at: None,
            }
        }

        fn answers(&mut self, now: DateTime<Utc>) -> bool {
            let answers = self
                .answered_at
                .is_none_or(|at| elapsed(at, now) >= self.tolerates);

            if answers {
                self.answered_at = Some(now);
            }

            answers
        }
    }

    /// One poll's outcome: whether it was refused, and the interval after it.
    type Outcome = (bool, Duration);

    /// Polls `provider` as often as `state` allows and not a moment later,
    /// moving the clock by hand from one attempt to the next.
    fn drive(state: &mut SourceState, provider: &mut Grudging, polls: usize) -> Vec<Outcome> {
        (0..polls)
            .map(|_| {
                let now = state.next_attempt;
                let refused = !provider.answers(now);

                assert!(state.ready_at(now));

                if refused {
                    state.wait_for_at(None, now);
                } else {
                    state.succeeded_at(now);
                }

                assert!(
                    state.interval() >= state.configured(),
                    "never below the floor"
                );

                (refused, state.interval())
            })
            .collect()
    }

    fn refusals(outcomes: &[Outcome]) -> usize {
        outcomes.iter().filter(|(refused, _)| *refused).count()
    }

    #[test]
    fn a_new_source_is_ready_and_has_never_connected() {
        let state = state();

        assert!(state.ready_at(at(0)), "the first poll happens immediately");
        assert!(!state.is_connected());
        assert!(!state.ever_connected());
        assert_eq!(state.last_error(), None);
        assert_eq!(state.interval(), Duration::from_secs(5));
        assert_eq!(state.configured(), Duration::from_secs(5));
        assert_eq!(state.mostly_refused(), None);
        assert_eq!(state.rate_limited(), 0);
    }

    #[test]
    fn a_successful_poll_holds_the_next_one_off_for_an_interval() {
        let mut state = state();

        state.succeeded_at(at(0));

        assert!(state.is_connected());
        assert!(state.ever_connected());
        assert!(!state.ready_at(at(4)), "five seconds have not passed");
        assert!(state.ready_at(at(5)));
        assert_eq!(state.reconnecting_for(), None);
    }

    #[test]
    fn the_backoff_doubles_and_then_stops_doubling() {
        let mut state = state();

        for (failures, expected) in [
            (1_u32, Duration::from_secs(5)),
            (2, Duration::from_secs(10)),
            (3, Duration::from_secs(20)),
            (4, Duration::from_secs(40)),
            (5, Duration::from_secs(80)),
            (6, Duration::from_secs(160)),
            (7, MAX_BACKOFF),
            (80, MAX_BACKOFF),
        ] {
            state.failures = failures;

            assert_eq!(state.backoff(), expected, "after {failures} failures");
        }
    }

    #[test]
    fn a_failure_remembers_what_went_wrong_and_that_it_never_worked() {
        let mut state = state();

        state.failed_at("the receiver refused the connection", at(0));

        assert!(!state.is_connected());
        assert!(!state.ever_connected(), "it has never answered");
        assert_eq!(
            state.last_error(),
            Some("the receiver refused the connection"),
        );
        assert!(state.reconnecting_for().is_some());
        assert!(!state.ready_at(at(4)));
    }

    #[test]
    fn recovering_clears_the_failure_and_the_backoff() {
        let mut state = state();

        state.failed_at("timed out", at(0));
        state.failed_at("timed out", at(10));
        state.succeeded_at(at(30));

        assert_eq!(state.failures, 0);
        assert_eq!(state.last_error(), None);
        assert!(state.is_connected());
        assert!(!state.outage.standing(), "and the run is over");
    }

    #[test]
    fn a_rate_limit_moves_the_schedule_without_marking_a_failure() {
        let mut state = state();
        state.succeeded_at(at(0));

        state.wait_for_at(Some(Duration::from_secs(60)), at(5));

        assert!(state.is_connected(), "429 is an upstream that is working");
        assert_eq!(state.last_error(), None);
        assert_eq!(state.rate_limited(), 1);
        assert!(!state.ready_at(at(64)));
        assert!(state.ready_at(at(65)));
    }

    #[test]
    fn a_rate_limit_never_asks_us_to_wait_longer_than_the_cap() {
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(86_400)), at(0));

        // A `Retry-After` of a day is either a mistake or a ban; either way a
        // sidecar that stopped polling until tomorrow would never notice it
        // being lifted.
        assert!(state.ready_at(at(0) + MAX_BACKOFF));
    }

    #[test]
    fn a_short_rate_limit_still_respects_the_configured_interval() {
        let mut state = SourceState::new("adsb.fi", Duration::from_secs(5));

        state.wait_for_at(Some(Duration::from_millis(100)), at(0));

        assert!(!state.ready_at(at(4)));
    }

    #[test]
    fn two_stated_rate_limits_inside_ten_polls_raise_the_interval_for_good() {
        // The Dublin finding: 429 with Retry-After: 10 on about every other
        // request at a five-second poll.
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(10)), at(0));

        assert_eq!(
            state.interval(),
            Duration::from_secs(5),
            "one 429 is a bad minute, not a rate limit",
        );

        state.succeeded_at(at(10));
        state.wait_for_at(Some(Duration::from_secs(10)), at(20));

        assert_eq!(
            state.interval(),
            Duration::from_secs(10),
            "the second one inside the window is the provider telling us its rate",
        );
        assert_eq!(state.rate_limited(), 2);

        // And it is never talked back down, by a smaller ask or by the clock.
        state.succeeded_at(at(30));
        state.wait_for_at(Some(Duration::from_secs(1)), at(40));
        state.wait_for_at(Some(Duration::from_secs(1)), at(50));

        assert_eq!(state.interval(), Duration::from_secs(10));

        // Nor by any number of clean polls: a delay the provider stated is a
        // floor, where one we guessed is only a position.
        let mut now = at(60);

        for _ in 0..(CLEAN_RUN * 5) {
            state.succeeded_at(now);
            now = state.next_attempt;
        }

        assert_eq!(state.interval(), Duration::from_secs(10));
        assert_eq!(
            state.configured(),
            Duration::from_secs(5),
            "what was configured is still reported as what was configured",
        );
    }

    #[test]
    fn two_rate_limits_further_apart_than_the_window_are_two_bad_minutes() {
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(10)), at(0));

        for second in 1..=(LIMIT_WINDOW as i64 + 1) {
            state.succeeded_at(at(second * 10));
        }

        state.wait_for_at(Some(Duration::from_secs(10)), at(500));

        assert_eq!(
            state.interval(),
            Duration::from_secs(5),
            "eleven polls apart is not a provider asking for a slower cadence",
        );
    }

    #[test]
    fn one_rate_limit_with_no_retry_after_waits_twice_the_interval_and_moves_nothing() {
        let mut state = state();

        state.wait_for_at(None, at(0));

        assert_eq!(
            state.interval(),
            Duration::from_secs(5),
            "one refusal is a bad minute, whether or not it named a delay",
        );
        assert!(!state.ready_at(at(9)), "it waits twice the interval");
        assert!(state.ready_at(at(10)));
    }

    #[test]
    fn a_second_one_inside_the_window_backs_the_cadence_off_by_half_as_much_again() {
        // The second Dublin finding. M9-08 adapted to none of this, because
        // none of it carried a `Retry-After`.
        let mut state = polling_every(10);

        state.wait_for_at(None, at(0));
        state.succeeded_at(at(20));
        state.wait_for_at(None, at(30));

        assert_eq!(state.interval(), Duration::from_secs(15));
        assert_eq!(state.configured(), Duration::from_secs(10));
        assert_eq!(state.rate_limited(), 2);
        assert!(!state.ready_at(at(59)), "and waits twice the new interval");
        assert!(state.ready_at(at(60)));

        state.succeeded_at(at(60));

        assert!(!state.ready_at(at(74)), "a clean poll is 15s from the next");
        assert!(state.ready_at(at(75)));
    }

    #[test]
    fn unstated_rate_limits_further_apart_than_the_window_move_nothing() {
        let mut state = polling_every(10);
        let mut now = at(0);

        for _ in 0..5 {
            state.wait_for_at(None, now);

            for _ in 0..=LIMIT_WINDOW {
                now = state.next_attempt;
                state.succeeded_at(now);
            }

            now = state.next_attempt;
        }

        assert_eq!(
            state.interval(),
            Duration::from_secs(10),
            "five bad minutes a day are not a rate limit",
        );
    }

    #[test]
    fn a_refusal_every_fifth_poll_walks_the_interval_up_to_the_ceiling_and_stops() {
        // What production looked like — six 429s in thirty polls — against a
        // provider that goes on refusing whatever we do.
        let mut state = polling_every(10);
        let mut seen = Vec::new();

        for _ in 0..10 {
            for _ in 0..4 {
                state.succeeded_at(state.next_attempt);
            }

            state.wait_for_at(None, state.next_attempt);
            seen.push(state.interval().as_secs());
        }

        assert_eq!(seen, [10, 15, 23, 35, 53, 80, 120, 120, 120, 120]);
        assert_eq!(state.interval(), MAX_ADAPTED);
        assert_eq!(state.mostly_refused(), None, "one in five is not most");
    }

    #[test]
    fn a_provider_that_tolerates_a_request_every_18s_stops_refusing_us() {
        // Polled every 10s with no adaptation, this provider refuses every
        // other request for ever, which is what M9-08 did against adsb.lol.
        let mut provider = Grudging::tolerating(18);
        let mut state = polling_every(10);

        let outcomes = drive(&mut state, &mut provider, 30);

        assert_eq!(
            refusals(&outcomes[..6]),
            3,
            "refused on the way up: at 10s, at 10s again, and at 15s",
        );
        assert_eq!(
            refusals(&outcomes[6..]),
            0,
            "and not once in the twenty-four polls after it settled",
        );
        assert_eq!(
            state.interval(),
            Duration::from_secs(23),
            "the first rung of the ladder the provider will put up with",
        );
        assert!(state.interval() >= provider.tolerates);
        assert_eq!(state.configured(), Duration::from_secs(10));
        assert_eq!(state.mostly_refused(), None);
    }

    #[test]
    fn over_a_thousand_polls_it_probes_downward_now_and_then_and_is_rarely_refused() {
        // AIMD does not find a number and keep it: every sixty clean polls it
        // tries one notch faster, is refused twice at 15s, and comes back to
        // 23s. Two refusals in sixty-odd polls is the price of noticing when
        // the provider's limit has gone away.
        let mut provider = Grudging::tolerating(18);
        let mut state = polling_every(10);

        let outcomes = drive(&mut state, &mut provider, 1_000);
        let (settling, settled) = outcomes.split_at(6);

        assert!(
            refusals(settled) * 20 < settled.len(),
            "{} refusals in {} polls is more than one in twenty",
            refusals(settled),
            settled.len(),
        );
        assert!(
            settled
                .iter()
                .all(|(_, interval)| (15..=23).contains(&interval.as_secs())),
            "it never runs away upwards, and never goes back to 10s against this provider",
        );
        assert_eq!(refusals(settling), 3);
    }

    #[test]
    fn sixty_clean_polls_ease_the_interval_back_and_never_below_what_was_configured() {
        let mut state = polling_every(10);

        for _ in 0..3 {
            state.wait_for_at(None, state.next_attempt);
        }

        assert_eq!(state.interval(), Duration::from_secs(23));

        let mut clean = |polls: u64| {
            for _ in 0..polls {
                state.succeeded_at(state.next_attempt);
            }

            state.interval()
        };

        assert_eq!(clean(CLEAN_RUN - 1), Duration::from_secs(23));
        assert_eq!(
            clean(1),
            Duration::from_secs(15),
            "the sixtieth earns a step"
        );
        assert_eq!(clean(CLEAN_RUN), Duration::from_secs(15 * 2 / 3));
        assert_eq!(
            clean(CLEAN_RUN * 10),
            Duration::from_secs(10),
            "an operator's `poll` is a floor: recovery stops there",
        );
    }

    #[test]
    fn a_refusal_in_the_middle_of_a_clean_run_starts_the_count_again() {
        let mut state = polling_every(10);

        state.wait_for_at(None, state.next_attempt);
        state.wait_for_at(None, state.next_attempt);

        for _ in 0..(CLEAN_RUN - 1) {
            state.succeeded_at(state.next_attempt);
        }

        // Outside the window of the last one, so it raises nothing either.
        state.wait_for_at(None, state.next_attempt);

        for _ in 0..(CLEAN_RUN - 1) {
            state.succeeded_at(state.next_attempt);
        }

        assert_eq!(state.interval(), Duration::from_secs(15), "not yet");

        state.succeeded_at(state.next_attempt);

        assert_eq!(state.interval(), Duration::from_secs(10));
    }

    #[test]
    fn a_source_refused_more_often_than_answered_says_how_often() {
        let mut state = polling_every(10);

        state.succeeded_at(state.next_attempt);

        for _ in 0..11 {
            state.wait_for_at(None, state.next_attempt);
        }

        assert_eq!(state.mostly_refused(), Some((11, 12)));
    }

    #[test]
    fn a_provider_that_refuses_every_other_request_is_one_run_of_notices() {
        // What the operator sees, rather than what the plugin does: twelve
        // notices in five minutes was the complaint.
        let mut state = state();

        state.wait_for_at(Some(Duration::from_secs(10)), at(0));

        assert!(state.limit.standing());

        for poll in 1..24 {
            state.succeeded_at(at(poll * 10));
            state.wait_for_at(Some(Duration::from_secs(10)), at(poll * 10 + 5));
        }

        assert!(
            state.limit.standing(),
            "a success in between does not end a run of rate limiting",
        );

        // Five minutes of not being refused is the end of it.
        state.succeeded_at(at(1_000));

        assert!(!state.limit.standing());
    }
}
