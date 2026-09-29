//! What the cadence remembers about the rungs it has already tried.
//!
//! A clean run earns a step down; that step is a **probe**, an experiment about
//! whether the provider's limit has moved. When the provider refuses the probe
//! within a few polls, that is an answer, and asking the same question again
//! sixty polls later — for ever — is not learning anything. So a refused rung
//! is remembered, and each consecutive refusal of it doubles how long a clean
//! run must be before it is tried again: 60 polls, 120, 240, and so on, up to
//! what fits in [`MAX_WAIT`] at the interval the source is resting at.
//!
//! A probe that holds (a full [`CLEAN_RUN`] at the lower rung) is the provider
//! answering the other way, and forgets. Nothing here is written down: a new
//! [`Cadence`](super::Cadence) — a restart, a changed `poll`, another
//! provider — starts with a clean slate.

use std::time::Duration;

use super::cadence::CLEAN_RUN;

/// The longest a refused rung is left alone, measured at the resting interval.
///
/// Three hours: long enough that a provider which refuses every probe costs
/// well under one refusal an hour, short enough that one whose limit has
/// relaxed is followed down the same afternoon.
pub const MAX_WAIT: Duration = Duration::from_secs(3 * 3600);

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
}

impl Probes {
    /// Nothing tried, nothing refused.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            probing: None,
            refused: None,
        }
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
                let cap = (MAX_WAIT.as_secs() / resting.as_secs().max(1)).max(CLEAN_RUN);

                CLEAN_RUN
                    .checked_shl(refused.times.min(32))
                    .map_or(cap, |wait| wait.min(cap))
            }
            _ => CLEAN_RUN,
        }
    }
}
