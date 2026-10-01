//! Time as the sketch keeps it: whole seconds since the limiter started.
//!
//! A cell has 32 bits for a moment and 16 for a window, so the limiter counts
//! from its own epoch — the moment it was built — rather than from 1970. Every
//! public method takes its moment from the caller (the `*_at` variants), so the
//! tests inject time and never sleep.
//!
//! Windows are a fixed grid from the epoch, not one per key: a sketch has no
//! per-key state to hang a "first failure" on. A burst straddling a boundary
//! can therefore land up to twice the allowance less one before it locks —
//! which the old per-key window allowed too, at its own boundary.

use chrono::{DateTime, Duration, Utc};

/// A moment, as a cell records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    /// Whole seconds since the epoch, rounded down.
    pub secs: u32,
    /// Which window of the grid `secs` falls in, modulo 2¹⁶.
    pub window: u16,
}

/// Converts between the caller's time and the sketch's.
#[derive(Debug, Clone, Copy)]
pub(super) struct Clock {
    epoch: DateTime<Utc>,
    /// The window, in whole seconds, at least one.
    window: u32,
    /// The lockout, in whole seconds rounded up, at least one.
    lockout: u32,
}

impl Clock {
    /// A clock whose second zero is `epoch`.
    pub fn new(epoch: DateTime<Utc>, window: Duration, lockout: Duration) -> Self {
        let seconds = |duration: Duration, round_up: bool| {
            let millis = duration.num_milliseconds().max(0);
            let whole = if round_up {
                millis.saturating_add(999) / 1000
            } else {
                millis / 1000
            };

            u32::try_from(whole).unwrap_or(u32::MAX).max(1)
        };

        Self {
            epoch,
            window: seconds(window, false),
            lockout: seconds(lockout, true),
        }
    }

    /// `now` as a cell records it. A moment before the epoch is second zero.
    pub fn stamp(&self, now: DateTime<Utc>) -> Stamp {
        let secs = u32::try_from((now - self.epoch).num_seconds().max(0)).unwrap_or(u32::MAX);
        let window = (secs / self.window) & u32::from(u16::MAX);

        Stamp {
            secs,
            // Masked to sixteen bits on the line above.
            window: u16::try_from(window).unwrap_or_default(),
        }
    }

    /// When a lockout that starts at `stamp` ends, in the sketch's seconds.
    pub fn lockout_ends(&self, stamp: Stamp) -> u32 {
        stamp.secs.saturating_add(self.lockout)
    }

    /// The lockout, in the sketch's seconds.
    pub fn lockout(&self) -> u32 {
        self.lockout
    }

    /// A sketch second as the caller's time.
    pub fn at(&self, secs: u32) -> DateTime<Utc> {
        self.epoch + Duration::seconds(i64::from(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> (Clock, DateTime<Utc>) {
        let epoch = Utc::now();

        (
            Clock::new(epoch, Duration::minutes(1), Duration::minutes(15)),
            epoch,
        )
    }

    #[test]
    fn the_grid_starts_at_the_epoch_and_moves_once_a_window() {
        let (clock, epoch) = clock();

        assert_eq!(clock.stamp(epoch), Stamp { secs: 0, window: 0 });
        assert_eq!(clock.stamp(epoch + Duration::seconds(59)).window, 0);
        assert_eq!(clock.stamp(epoch + Duration::seconds(60)).window, 1);
        assert_eq!(clock.stamp(epoch - Duration::hours(1)).secs, 0);
    }

    #[test]
    fn a_lockout_ends_a_whole_lockout_after_it_began_and_converts_back() {
        let (clock, epoch) = clock();
        let stamp = clock.stamp(epoch + Duration::milliseconds(10_500));

        assert_eq!(stamp.secs, 10);
        assert_eq!(clock.lockout_ends(stamp), 910);
        assert_eq!(clock.at(910), epoch + Duration::seconds(910));
    }

    #[test]
    fn a_sub_second_setting_is_a_second_and_a_lockout_rounds_up() {
        let clock = Clock::new(
            Utc::now(),
            Duration::milliseconds(30),
            Duration::milliseconds(1_200),
        );

        assert_eq!(clock.window, 1);
        assert_eq!(clock.lockout(), 2);
    }
}
