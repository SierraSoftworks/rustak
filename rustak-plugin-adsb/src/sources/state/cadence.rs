//! How often a source may ask, and everything that moves that number.
//!
//! # Why this is not just the number in the file
//!
//! M9-08 taught [`SourceState`](super::SourceState) to take a provider at its
//! word: a `429` carrying `Retry-After: 10`, twice inside [`LIMIT_WINDOW`]
//! polls, makes ten seconds the cadence. That only ever fires for a delay the
//! provider **stated**, and the second live deployment found the case it
//! misses. adsb.lol answered `429` six times in thirty polls and named no delay
//! at all — no `Retry-After`, in any form — so nothing adapted, the feed kept
//! being refused at the same rate for as long as it ran, and the one line the
//! operator saw reported our own fallback guess as though the server had asked
//! for it.
//!
//! A provider that refuses without saying how long is the common case, not the
//! exception: adsb.lol documents its limits as "dynamic based on the
//! environment load", so a stated delay may never come.
//!
//! # So it backs off, and it comes back
//!
//! M9-08 excluded unstated refusals for a good reason — a guess that becomes
//! the cadence compounds, and a week of flaky minutes would walk a feed up to
//! the ceiling and leave it there. The answer is not to refuse to adapt, it is
//! to make the adaptation **reversible**:
//!
//! - a refusal that names no delay, arriving within [`LIMIT_WINDOW`] polls of
//!   the last one, multiplies the interval by 1.5 (rounded up to a whole
//!   second, capped at [`MAX_ADAPTED`]);
//! - [`CLEAN_RUN`] consecutive polls that nobody refused divide it by 1.5
//!   again, one notch at a time, and any refusal resets that count;
//! - it never goes below [`Cadence::floor`]: the configured or provider-default
//!   interval, or a delay the provider itself stated, whichever is longer.
//!
//! A flaky minute therefore costs a few minutes of polling slightly slower, and
//! a provider that keeps refusing settles at the rate it will actually tolerate
//! — which is the number nobody published.
//!
//! An operator's `poll` is the **floor** of all this, not a pin: it is where a
//! source starts and the fastest it will ever come back to, and server
//! behaviour may put the effective cadence above it.

use std::time::Duration;

/// How many polls apart two refusals may be and still be one rate limit
/// rather than two bad minutes.
///
/// Ten: at any sane interval that is a minute or two of asking, which is short
/// enough that an unrelated pair does not slow a feed down for the rest of the
/// day and long enough that "every other request" is caught on the second one.
pub const LIMIT_WINDOW: u64 = 10;

/// The slowest an adapted cadence ever becomes.
///
/// Two minutes: a source that has walked itself up this far is being refused
/// consistently, and a feed asking any less often than this is not a live
/// picture any more. Well under
/// [`MAX_BACKOFF`](super::MAX_BACKOFF), which is what an
/// outage — a different thing — is allowed.
pub const MAX_ADAPTED: Duration = Duration::from_secs(120);

/// How many consecutive clean polls earn one step back down.
///
/// Sixty: at a ten-second cadence that is ten minutes of not being refused,
/// which is long enough that the step down is evidence rather than optimism and
/// short enough that a provider which had one bad afternoon does not cost a
/// slow feed for the rest of the day.
pub const CLEAN_RUN: u64 = 60;

/// How many recent polls a heartbeat judges a source by.
pub const RECENT_POLLS: u32 = 20;

/// The low [`RECENT_POLLS`] bits, which is the window [`Cadence`] keeps.
const RECENT_MASK: u32 = (1 << RECENT_POLLS) - 1;

/// What changed about the cadence, for the one line that says so.
///
/// Returned rather than logged here so that the suites can assert which change
/// a sequence of polls produces without reading a log, and so that the wording
/// stays in one place beside every other thing this plugin says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The provider named a delay, twice inside the window, and we took it at
    /// its word. Never decayed below afterwards.
    Stated(Duration),

    /// The provider refused again without naming a delay, so we backed off.
    Raised(Duration),

    /// Nobody has refused us in a long time, so we stepped back down.
    Eased {
        /// The interval now in use.
        interval: Duration,
        /// How many clean polls in a row earned it.
        after: u64,
    },
}

impl Change {
    /// The interval this change left in use.
    #[must_use]
    pub const fn interval(self) -> Duration {
        match self {
            Self::Stated(interval) | Self::Raised(interval) | Self::Eased { interval, .. } => {
                interval
            }
        }
    }
}

/// How often a source may reach its upstream, and the history that moves it.
#[derive(Clone, Debug)]
pub struct Cadence {
    /// What the operator asked for, or the provider's own default. The floor.
    configured: Duration,

    /// What is actually being used, which is never below [`Self::floor`].
    effective: Duration,

    /// The longest delay the provider ever stated and we adopted, which is a
    /// floor of its own: a number it gave us is not one we talk it out of.
    stated: Option<Duration>,

    /// How many attempts have been made, which is what [`LIMIT_WINDOW`] counts.
    polls: u64,

    /// The attempt the last refusal arrived on.
    limited_at: Option<u64>,

    /// How many polls in a row nobody has refused.
    clean: u64,

    /// The last [`RECENT_POLLS`] outcomes, one bit each, refusals set.
    recent: u32,
}

impl Cadence {
    /// A cadence that starts where the configuration put it.
    #[must_use]
    pub const fn new(configured: Duration) -> Self {
        Self {
            configured,
            effective: configured,
            stated: None,
            polls: 0,
            limited_at: None,
            clean: 0,
            recent: 0,
        }
    }

    /// What the operator asked for, or the provider's own default.
    #[must_use]
    pub const fn configured(&self) -> Duration {
        self.configured
    }

    /// The cadence as it stands, which is what the next poll waits.
    #[must_use]
    pub const fn effective(&self) -> Duration {
        self.effective
    }

    /// How many of the last [`RECENT_POLLS`] polls were refused.
    #[must_use]
    pub const fn refused_recently(&self) -> u32 {
        self.recent.count_ones()
    }

    /// How many polls that is, before [`RECENT_POLLS`] of them have happened.
    #[must_use]
    pub const fn recent_polls(&self) -> u32 {
        if self.polls >= RECENT_POLLS as u64 {
            RECENT_POLLS
        } else {
            self.polls as u32
        }
    }

    /// Records a poll that worked, and eases the cadence after a long enough
    /// run of them.
    pub fn succeeded(&mut self) -> Option<Change> {
        self.record(false);
        self.clean = self.clean.saturating_add(1);

        if self.clean < CLEAN_RUN {
            return None;
        }

        let after = std::mem::replace(&mut self.clean, 0);
        let eased = eased(self.effective).max(self.floor());

        if eased >= self.effective {
            return None;
        }

        self.effective = eased;

        Some(Change::Eased {
            interval: eased,
            after,
        })
    }

    /// Records a poll that failed for a reason that is not a refusal.
    ///
    /// It is neither clean nor a refusal, so it moves nothing: an upstream we
    /// could not reach is no evidence about the rate it would have tolerated.
    pub fn failed(&mut self) {
        self.record(false);
    }

    /// Records a refusal, and answers how the cadence moved because of it.
    ///
    /// `asked` is the delay the response stated, when it stated one — already
    /// clamped by the caller to something a sidecar can sensibly wait.
    pub fn refused(&mut self, asked: Option<Duration>) -> Option<Change> {
        self.record(true);
        self.clean = 0;

        let again = self
            .limited_at
            .is_some_and(|at| self.polls.saturating_sub(at) <= LIMIT_WINDOW);

        self.limited_at = Some(self.polls);

        // One refusal is a bad minute. Two inside the window is the provider
        // telling us something, whether or not it used a header to do it.
        if !again {
            return None;
        }

        match asked {
            Some(stated) => self.take_at_its_word(stated),
            None => self.back_off(),
        }
    }

    /// The fastest this cadence may ever come back to: what was configured, or
    /// a delay the provider itself stated, whichever is longer.
    ///
    /// Deliberately not capped at [`MAX_ADAPTED`]. That ceiling is on our own
    /// guessing; a provider that said "every three minutes", twice, is not
    /// eased back under three minutes because we would have stopped at two.
    #[must_use]
    pub fn floor(&self) -> Duration {
        self.configured.max(self.stated.unwrap_or(Duration::ZERO))
    }

    /// How many of how many recent polls were refused, when that is **more
    /// than half of the last [`RECENT_POLLS`]** — which is the point at which
    /// rate limiting stops being something this plugin absorbs and becomes
    /// something an administrator should hear about.
    #[must_use]
    pub const fn mostly_refused(&self) -> Option<(u32, u32)> {
        let refused = self.refused_recently();

        if refused * 2 > RECENT_POLLS {
            Some((refused, self.recent_polls()))
        } else {
            None
        }
    }

    /// M9-08's rule: a delay the provider stated twice becomes the cadence, and
    /// a floor under it.
    fn take_at_its_word(&mut self, stated: Duration) -> Option<Change> {
        // `max`, never `min`: a provider that has told us twice is not talked
        // back down by one that asks for less later, nor by the clock.
        self.stated = Some(self.stated.map_or(stated, |held| held.max(stated)));

        if stated <= self.effective {
            return None;
        }

        self.effective = stated;

        Some(Change::Stated(stated))
    }

    /// The unstated case: multiply, and let a clean run divide it back.
    fn back_off(&mut self) -> Option<Change> {
        let raised = raised(self.effective);

        if raised <= self.effective {
            return None;
        }

        self.effective = raised;

        Some(Change::Raised(raised))
    }

    /// Counts one poll, and remembers whether it was refused.
    fn record(&mut self, refused: bool) {
        self.polls = self.polls.saturating_add(1);
        self.recent = ((self.recent << 1) | u32::from(refused)) & RECENT_MASK;
    }
}

/// One notch slower: ×1.5 in whole seconds, always at least a second more, and
/// never past [`MAX_ADAPTED`].
fn raised(from: Duration) -> Duration {
    let seconds = from.as_secs();
    let raised = seconds
        .saturating_mul(3)
        .div_ceil(2)
        .max(seconds.saturating_add(1));

    Duration::from_secs(raised).min(MAX_ADAPTED)
}

/// One notch faster: ÷1.5 in whole seconds, rounded down. From a ten-second
/// start that walks back down exactly the ladder [`raised`] walked up (120, 80,
/// 53, 35, 23, 15, 10); from anywhere else the last step lands on
/// [`Cadence::floor`], because the caller never lets it go below.
fn eased(from: Duration) -> Duration {
    Duration::from_secs(from.as_secs() * 2 / 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cadence() -> Cadence {
        Cadence::new(Duration::from_secs(10))
    }

    /// One refusal, then another on the very next poll: the pair that turns a
    /// bad minute into a rate limit. Only the *first* pair needs two — every
    /// refusal after it is already inside the window of the one before.
    fn refused_twice(cadence: &mut Cadence, asked: Option<Duration>) -> Option<Change> {
        cadence.refused(asked);
        cadence.refused(asked)
    }

    #[test]
    fn a_new_cadence_is_the_one_that_was_configured() {
        let cadence = cadence();

        assert_eq!(cadence.configured(), Duration::from_secs(10));
        assert_eq!(cadence.effective(), Duration::from_secs(10));
        assert_eq!(cadence.refused_recently(), 0);
        assert_eq!(cadence.recent_polls(), 0);
    }

    #[test]
    fn one_refusal_that_names_no_delay_is_a_bad_minute_and_moves_nothing() {
        let mut cadence = cadence();

        assert_eq!(cadence.refused(None), None);
        assert_eq!(cadence.effective(), Duration::from_secs(10));
    }

    #[test]
    fn a_second_unstated_refusal_inside_the_window_backs_the_cadence_off() {
        // The Dublin finding: six 429s in thirty polls, no Retry-After in any
        // of them, and M9-08 adapting to none of it.
        let mut cadence = cadence();

        assert_eq!(
            refused_twice(&mut cadence, None),
            Some(Change::Raised(Duration::from_secs(15))),
        );
        assert_eq!(cadence.effective(), Duration::from_secs(15));
    }

    #[test]
    fn unstated_refusals_further_apart_than_the_window_are_two_bad_minutes() {
        let mut cadence = cadence();

        cadence.refused(None);

        for _ in 0..=LIMIT_WINDOW {
            cadence.succeeded();
        }

        assert_eq!(cadence.refused(None), None);
        assert_eq!(cadence.effective(), Duration::from_secs(10));
    }

    #[test]
    fn backing_off_walks_up_a_ladder_and_stops_at_the_ceiling() {
        let mut cadence = cadence();

        assert_eq!(cadence.refused(None), None, "the first is a bad minute");

        for expected in [15, 23, 35, 53, 80, 120] {
            assert_eq!(
                cadence.refused(None),
                Some(Change::Raised(Duration::from_secs(expected))),
                "from {:?}",
                cadence.effective(),
            );
        }

        assert_eq!(
            cadence.refused(None),
            None,
            "the ceiling is not a change to announce over and over",
        );
        assert_eq!(cadence.effective(), MAX_ADAPTED);
    }

    #[test]
    fn a_clean_run_walks_back_down_the_same_ladder_and_stops_at_the_floor() {
        let mut cadence = cadence();

        refused_twice(&mut cadence, None);
        cadence.refused(None);

        assert_eq!(cadence.effective(), Duration::from_secs(23));

        for expected in [15, 10] {
            let change = (0..CLEAN_RUN).find_map(|_| cadence.succeeded());

            assert_eq!(
                change,
                Some(Change::Eased {
                    interval: Duration::from_secs(expected),
                    after: CLEAN_RUN,
                }),
            );
        }

        assert_eq!(
            (0..CLEAN_RUN * 3).find_map(|_| cadence.succeeded()),
            None,
            "the configured interval is a floor, not a waypoint",
        );
        assert_eq!(cadence.effective(), Duration::from_secs(10));
    }

    #[test]
    fn a_refusal_resets_the_clean_run() {
        let mut cadence = cadence();

        refused_twice(&mut cadence, None);

        for _ in 0..(CLEAN_RUN - 1) {
            assert_eq!(cadence.succeeded(), None);
        }

        cadence.refused(None);

        for _ in 0..(CLEAN_RUN - 1) {
            assert_eq!(cadence.succeeded(), None, "the count started again");
        }

        assert!(cadence.succeeded().is_some());
    }

    #[test]
    fn a_stated_delay_follows_m9_08s_rule_and_is_never_decayed_below() {
        let mut cadence = cadence();

        assert_eq!(cadence.refused(Some(Duration::from_secs(30))), None);
        assert_eq!(
            cadence.refused(Some(Duration::from_secs(30))),
            Some(Change::Stated(Duration::from_secs(30))),
        );

        for _ in 0..(CLEAN_RUN * 5) {
            assert_eq!(
                cadence.succeeded(),
                None,
                "a number the provider gave us is not one a clean run talks it out of",
            );
        }

        assert_eq!(cadence.effective(), Duration::from_secs(30));
    }

    #[test]
    fn a_cadence_raised_by_guessing_still_eases_back_under_a_stated_floor() {
        let mut cadence = cadence();

        cadence.refused(Some(Duration::from_secs(20)));
        cadence.refused(Some(Duration::from_secs(20)));

        assert_eq!(
            cadence.refused(None),
            Some(Change::Raised(Duration::from_secs(30))),
            "still inside the window, and this time it did not say how long",
        );

        assert_eq!(
            (0..CLEAN_RUN).find_map(|_| cadence.succeeded()),
            Some(Change::Eased {
                interval: Duration::from_secs(20),
                after: CLEAN_RUN,
            }),
        );
        assert_eq!(
            (0..CLEAN_RUN * 3).find_map(|_| cadence.succeeded()),
            None,
            "20s is what it asked for, and that is where the decay stops",
        );
    }

    #[test]
    fn a_stated_delay_above_our_own_ceiling_is_still_never_decayed_below() {
        // MAX_ADAPTED caps what we guess, not what we were told.
        let mut cadence = cadence();
        let stated = Duration::from_secs(200);

        cadence.refused(Some(stated));

        assert_eq!(cadence.refused(Some(stated)), Some(Change::Stated(stated)));
        assert_eq!(cadence.floor(), stated);
        assert_eq!((0..CLEAN_RUN * 3).find_map(|_| cadence.succeeded()), None);
        assert_eq!(cadence.effective(), stated);
        assert_eq!(
            cadence.refused(None),
            None,
            "and guessing never raises it further, or lowers it to the ceiling",
        );
        assert_eq!(cadence.effective(), stated);
    }

    #[test]
    fn a_five_second_cadence_eases_back_to_five_and_not_to_four() {
        // 5 → 8 → 12 on the way up; 12 → 8 → 5 on the way down, where a bare
        // ÷1.5 of 8 would be 5.33 and of 6 would be 4.
        let mut cadence = Cadence::new(Duration::from_secs(5));

        refused_twice(&mut cadence, None);
        cadence.refused(None);

        assert_eq!(cadence.effective(), Duration::from_secs(12));

        for expected in [8, 5] {
            assert_eq!(
                (0..CLEAN_RUN)
                    .find_map(|_| cadence.succeeded())
                    .map(Change::interval),
                Some(Duration::from_secs(expected)),
            );
        }

        assert_eq!((0..CLEAN_RUN * 3).find_map(|_| cadence.succeeded()), None);
        assert_eq!(cadence.effective(), cadence.configured());
    }

    #[test]
    fn more_than_half_of_the_last_twenty_refused_is_worth_telling_somebody() {
        let mut cadence = cadence();

        for _ in 0..10 {
            cadence.refused(None);
            cadence.succeeded();
        }

        assert_eq!(
            cadence.mostly_refused(),
            None,
            "ten of twenty is half, and half is what adapting is for",
        );

        // The window slides: the refusal that arrives pushes the oldest poll
        // out, and the oldest poll here was a refusal too.
        cadence.refused(None);

        assert_eq!(cadence.mostly_refused(), None);

        cadence.refused(None);

        assert_eq!(cadence.mostly_refused(), Some((11, RECENT_POLLS)));

        let mut young = Cadence::new(Duration::from_secs(10));

        for _ in 0..8 {
            young.refused(None);
        }

        assert_eq!(
            young.mostly_refused(),
            None,
            "eight polls are not yet more than half of twenty",
        );
    }

    #[test]
    fn a_smaller_stated_delay_does_not_talk_the_cadence_back_down() {
        let mut cadence = cadence();

        cadence.refused(Some(Duration::from_secs(40)));
        cadence.refused(Some(Duration::from_secs(40)));
        cadence.refused(Some(Duration::from_secs(1)));

        assert_eq!(cadence.effective(), Duration::from_secs(40));
    }

    #[test]
    fn the_recent_window_is_the_last_twenty_polls_and_nothing_older() {
        let mut cadence = cadence();

        for _ in 0..30 {
            cadence.refused(None);
        }

        assert_eq!(cadence.recent_polls(), RECENT_POLLS);
        assert_eq!(cadence.refused_recently(), RECENT_POLLS);

        for _ in 0..RECENT_POLLS {
            cadence.succeeded();
        }

        assert_eq!(
            cadence.refused_recently(),
            0,
            "twenty clean polls is a clean window, whatever came before it",
        );
    }

    #[test]
    fn a_failed_poll_is_neither_clean_nor_a_refusal() {
        let mut cadence = cadence();

        refused_twice(&mut cadence, None);

        for _ in 0..5 {
            cadence.failed();
        }

        assert_eq!(cadence.refused_recently(), 2, "a failure is not a refusal");
        assert_eq!(cadence.recent_polls(), 7, "but it is a poll");

        for _ in 0..CLEAN_RUN {
            cadence.failed();
        }

        assert_eq!(
            cadence.effective(),
            Duration::from_secs(15),
            "an upstream we could not reach says nothing about its rate limit",
        );
    }
}
