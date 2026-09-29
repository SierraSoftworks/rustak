//! What the cadence remembers about the rungs it has already tried.
//!
//! A clean run earns a step down; that step is a **probe**, an experiment about
//! whether the provider's limit has moved. When the provider refuses the probe
//! within a few polls, that is an answer, and asking the same question again
//! sixty polls later — for ever — is not learning anything. So a refused rung
//! is remembered, and each consecutive refusal of it doubles how long a clean
//! run must be before it is tried again: 60 polls, 120, 240, and so on, up to
//! what fits in the longest wait ([`DEFAULT_MAX_WAIT`] unless an operator set
//! `probe_max_wait`) at the interval the source is resting at.
//!
//! A probe that holds (a full [`CLEAN_RUN`] at the lower rung) is the provider
//! answering the other way, and forgets. Nothing here is written down: a new
//! [`Cadence`](super::Cadence) — a restart, a changed `poll`, another
//! provider — starts with a clean slate.

use std::time::Duration;

use super::cadence::CLEAN_RUN;

/// The longest a refused rung is left alone, measured at the resting interval,
/// when nobody has said otherwise.
///
/// Three hours: long enough that a provider which refuses every probe costs
/// well under one refusal an hour, short enough that one whose limit has
/// relaxed is followed down the same afternoon. An operator who would rather
/// be refused less often (six hours roughly halves it) or follow a relaxed
/// limit sooner sets `probe_max_wait`, within [`LONGEST_MAX_WAIT`] and
/// [`shortest_max_wait`].
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(3 * 3600);

/// The longest `probe_max_wait` may be: a day. Longer, and a provider that
/// relaxed its limit on Monday is followed down some time on Wednesday.
pub const LONGEST_MAX_WAIT: Duration = Duration::from_secs(24 * 3600);

/// The shortest `probe_max_wait` may be for a source configured to ask every
/// `configured`: one clean run at that interval. Any shorter would be no wait
/// at all, since a step down always needs one clean run.
#[must_use]
pub fn shortest_max_wait(configured: Duration) -> Duration {
    configured.saturating_mul(u32::try_from(CLEAN_RUN).unwrap_or(u32::MAX))
}

/// Why `wait` is not a usable `probe_max_wait` for a source configured to ask
/// every `configured`, or [`None`] when it is. `key` is the name the reader
/// wrote it under: `probe_max_wait` in the file, `probe_max_wait_minutes` on
/// the Services page, which is why each bound is given in both units.
#[must_use]
pub fn out_of_range(key: &str, wait: Duration, configured: Duration) -> Option<String> {
    let shortest = shortest_max_wait(configured);

    if wait > LONGEST_MAX_WAIT {
        Some(format!(
            "`{key}` is {}, which is longer than a day; a provider that relaxed its limit \
             would not be noticed for days. Choose at most {}.",
            humane(wait),
            both(LONGEST_MAX_WAIT),
        ))
    } else if wait < shortest {
        Some(format!(
            "`{key}` is {}, which is shorter than one clean run of {CLEAN_RUN} polls at the \
             configured {}. Choose at least {}.",
            humane(wait),
            humane(configured),
            both(shortest),
        ))
    } else {
        None
    }
}

/// A bound as a file writes it and as a whole number of minutes, rounded up.
fn both(span: Duration) -> String {
    format!("{} ({} minutes)", humane(span), span.as_secs().div_ceil(60))
}

/// A span the way the configuration file would write it: `"3h"`, `"90m"`.
fn humane(span: Duration) -> String {
    chrono::Duration::from_std(span)
        .ok()
        .and_then(|span| rustak_core::config::duration::format(span).ok())
        .unwrap_or_else(|| format!("{}s", span.as_secs()))
}

/// A rung that was refused, and how many probes of it in a row were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Refused {
    rung: Duration,
    times: u32,
}

/// The probe in progress, if any, and the rung that last refused one.
#[derive(Clone, Debug)]
pub struct Probes {
    /// The rung a step down has just moved to, until it has held or been
    /// refused.
    probing: Option<Duration>,
    refused: Option<Refused>,

    /// The longest a refused rung is left alone.
    max_wait: Duration,
}

impl Probes {
    /// Nothing tried, nothing refused, and the default longest wait.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            probing: None,
            refused: None,
            max_wait: DEFAULT_MAX_WAIT,
        }
    }

    /// The longest a refused rung is left alone.
    #[must_use]
    pub const fn max_wait(&self) -> Duration {
        self.max_wait
    }

    /// Changes the longest wait. Nothing remembered is forgotten: the next
    /// decision about a step down is simply measured against the new cap.
    pub const fn set_max_wait(&mut self, max_wait: Duration) {
        self.max_wait = max_wait;
    }

    /// A step down to `rung` was just taken.
    pub const fn began(&mut self, rung: Duration) {
        self.probing = Some(rung);
    }

    /// A full clean run happened at whatever is being probed: the provider
    /// tolerates that rung, and anything remembered at or above it is stale.
    pub fn held(&mut self) {
        let Some(rung) = self.probing.take() else {
            return;
        };

        if self.refused.is_some_and(|refused| refused.rung >= rung) {
            self.refused = None;
        }
    }

    /// The cadence rose again while a probe was in progress, so the provider
    /// refused it. Answers the probed rung and how many polls of clean running
    /// it will now take before it is tried again, or [`None`] if no probe was
    /// in progress and this was an ordinary refusal.
    pub fn refused(&mut self, resting: Duration) -> Option<(Duration, u64)> {
        let rung = self.probing.take()?;
        let times = match self.refused {
            Some(before) if before.rung == rung => before.times.saturating_add(1),
            _ => 1,
        };

        self.refused = Some(Refused { rung, times });

        Some((rung, self.required(rung, resting)))
    }

    /// The provider's own words changed the floor: nothing learnt before is
    /// safe to lean on.
    pub const fn forget(&mut self) {
        self.probing = None;
        self.refused = None;
    }

    /// How many clean polls at `resting` earn a step down to `next`.
    #[must_use]
    pub fn required(&self, next: Duration, resting: Duration) -> u64 {
        match self.refused {
            Some(refused) if refused.rung == next => {
                let cap = (self.max_wait.as_secs() / resting.as_secs().max(1)).max(CLEAN_RUN);

                CLEAN_RUN
                    .checked_shl(refused.times.min(32))
                    .map_or(cap, |wait| wait.min(cap))
            }
            _ => CLEAN_RUN,
        }
    }
}
