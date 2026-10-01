//! One tier of the limiter: a count-min sketch whose cells also lock.
//!
//! # Shape and size
//!
//! [`DEPTH`] rows of a power-of-two width ([`WIDTH`] in production), each cell
//! one `AtomicU64`, allocated once and zeroed. A key is a 64-bit keyed hash
//! ([`super::key`]); its cell in row `i` is `(h₁ + i·h₂) mod width`, with `h₁`
//! and `h₂` the low and high halves of that one hash and `h₂` forced odd —
//! Kirsch and Mitzenmacher's double hashing, which costs one hash per
//! operation and loses nothing measurable against `DEPTH` independent ones.
//!
//! `DEPTH × WIDTH = 4 × 2¹⁶` cells is 2 MiB a tier. The cost of a false
//! refusal is the chance that every one of a fresh key's cells is locked; with
//! `K` keys locked at once that is about `(1 − e^(−K/WIDTH))^DEPTH`, which the
//! simulation in `simulation_tests.rs` measures: none of 50 000 fresh keys at
//! a thousand lockouts, about 0.25 % at 16 384, 2.4 % at 32 768 and 16 % at
//! 65 536. A real installation has a handful of lockouts. Tier 1 caps what one
//! caller can add: at the default allowances (300 failures a window per
//! address, 3 000 per IPv6 /48, 10 per pair) one IPv4 address locks about 30
//! pairs before it is itself locked for the lockout, and one IPv6 /48 about
//! 300. So 16 384 lockouts in force (0.24 % false refusals) takes roughly 550
//! IPv4 addresses or 55 IPv6 /48s inside one lockout, and 65 536 (16 %) about
//! 2 200 or 220 — an IPv6 /32 alone holds 65 536 /48s. Four rows rather than
//! more keeps a check to four loads per key; doubling the width would halve
//! `K/WIDTH` for another 2 MiB a tier and buy only the flood case.
//!
//! # A cell
//!
//! `until:32 | count:16 | window:16`, so that every change is one
//! compare-and-swap of one word:
//!
//! * `count` — failures in `window`, saturating at `u16::MAX`;
//! * `window` — the window of the grid ([`super::clock`]) the count belongs to,
//!   modulo 2¹⁶. A cell from an older window reads as zero and is restarted by
//!   the next write, so windows run out per cell and nothing ever sweeps. A cell
//!   up to [`AHEAD`] windows *ahead* of the writer (a thread whose clock read is
//!   a moment older) is counted in its own window rather than wound back;
//! * `until` — the second the cell's lock ends, kept across windows.
//!
//! # Conservative update, and why it never undercounts
//!
//! A failure reads the key's cells, takes the least count `m`, and raises every
//! cell below `m + 1` to `m + 1` (Estan and Varghese), so a cell shared with a
//! busier key is not pushed further than this key needs. Each raise is a
//! compare-and-swap from the value read; if any one fails, another write got
//! there first and the whole update starts again from a fresh read. All of it
//! is `SeqCst`.
//!
//! **Property.** If `N` failures of one key complete within one window, with no
//! write from a later window and no clear touching its cells meanwhile, every
//! one of its cells then holds at least `N` (up to the saturation at
//! `u16::MAX`).
//!
//! **Why.** Under those conditions each cell's count only grows. Take two
//! completed updates that read the same least count `m`. If they read a common
//! cell at `m`, both swapped it from a count-`m` value to `m + 1` or more, and
//! only one swap can follow a count-`m` value. If they did not, the first read
//! `X = m` and `Y > m`, the second `Y = m` and `X > m`; for both swaps to
//! succeed the second's swap on `Y` must precede the first's read of `Y`, and
//! the first's swap on `X` the second's read of `X` — a cycle against program
//! order that a single `SeqCst` order forbids. So completed updates read
//! distinct `m`, the largest is at least `N − 1`, and once it completed every
//! cell held at least `m + 1 ≥ N`. Retrying is lock-free: an attempt fails only
//! because some other write succeeded.
//!
//! A clear zeroes cells on purpose, and a clock stepped back more than
//! [`AHEAD`] windows restarts them; both forgive, which is the direction a
//! limiter can afford. A cell untouched for nearly 2¹⁶ windows (45 days at a
//! minute) can alias a window up to [`AHEAD`] ahead of the writer's and its old
//! count be read as current for those windows: an overcount, never an
//! undercount.

use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

use super::clock::Stamp;

/// Rows per tier.
pub const DEPTH: usize = 4;

/// Cells per row in production: see the module docs for the sizing.
pub const WIDTH: usize = 1 << 16;

/// How many windows ahead of a writer a cell may be and still be its own.
const AHEAD: u16 = 2;

/// How many cells [`Table::locked_fraction`] reads.
const SAMPLE: usize = 4096;

/// One cell, unpacked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Cell {
    pub until: u32,
    pub count: u16,
    pub window: u16,
}

impl Cell {
    fn pack(self) -> u64 {
        u64::from(self.until) | (u64::from(self.count) << 32) | (u64::from(self.window) << 48)
    }

    fn unpack(raw: u64) -> Self {
        // Each field is masked to its width before narrowing.
        Self {
            until: u32::try_from(raw & 0xFFFF_FFFF).unwrap_or_default(),
            count: u16::try_from((raw >> 32) & 0xFFFF).unwrap_or_default(),
            window: u16::try_from(raw >> 48).unwrap_or_default(),
        }
    }

    /// This cell as a writer in `window` sees it.
    fn in_window(self, window: u16) -> Self {
        if self.window.wrapping_sub(window) <= AHEAD {
            return self;
        }

        Self {
            count: 0,
            window,
            ..self
        }
    }
}

/// What one failure did to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Recorded {
    /// The key's count after this failure.
    pub estimate: u16,
    /// When the lockout this failure started ends; [`None`] if it started none.
    pub began: Option<u32>,
}

/// `DEPTH` rows of atomic cells.
pub(super) struct Table {
    cells: Box<[AtomicU64]>,
    width: usize,
}

impl Table {
    /// A zeroed table `width` cells wide, which must be a power of two.
    pub fn new(width: usize) -> Self {
        debug_assert!(width.is_power_of_two());

        Self {
            cells: (0..DEPTH * width).map(|_| AtomicU64::new(0)).collect(),
            width,
        }
    }

    /// The bytes the cells occupy, which never changes after [`Table::new`].
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(&*self.cells)
    }

    /// The key's cell in every row.
    fn slots(&self, hash: u64) -> [usize; DEPTH] {
        let first = hash & 0xFFFF_FFFF;
        let step = (hash >> 32) | 1;
        let mask = (self.width - 1) as u64;

        std::array::from_fn(|row| {
            // Masked to below `width`, which is a `usize`.
            let column =
                usize::try_from(first.wrapping_add(row as u64 * step) & mask).unwrap_or_default();

            row * self.width + column
        })
    }

    fn read(&self, slots: &[usize; DEPTH]) -> [u64; DEPTH] {
        slots.map(|slot| self.cells[slot].load(SeqCst))
    }

    /// When the key's lockout ends, if every one of its cells is locked at
    /// `now`: the least `until` across them, which is what refuses it.
    pub fn locked_until(&self, hash: u64, now: Stamp) -> Option<u32> {
        let until = self
            .read(&self.slots(hash))
            .iter()
            .map(|raw| Cell::unpack(*raw).until)
            .min()
            .unwrap_or_default();

        (until > now.secs).then_some(until)
    }

    /// The key's count in `window`: the least across its cells.
    pub fn estimate(&self, hash: u64, window: u16) -> u16 {
        self.read(&self.slots(hash))
            .iter()
            .map(|raw| Cell::unpack(*raw).in_window(window).count)
            .min()
            .unwrap_or_default()
    }

    /// Counts one failure, and locks the key until `until` if it reaches
    /// `threshold` while not already locked.
    pub fn record(&self, hash: u64, now: Stamp, threshold: u16, until: u32) -> Recorded {
        let slots = self.slots(hash);

        loop {
            let seen = self.read(&slots);
            let cells = seen.map(|raw| Cell::unpack(raw).in_window(now.window));
            let least = cells
                .iter()
                .map(|cell| cell.count)
                .min()
                .unwrap_or_default();
            let locked = cells
                .iter()
                .map(|cell| cell.until)
                .min()
                .unwrap_or_default()
                > now.secs;
            let estimate = least.saturating_add(1);
            let began = (!locked && estimate >= threshold).then_some(until);

            if self.raise(&slots, &seen, &cells, estimate, began) {
                return Recorded { estimate, began };
            }
        }
    }

    /// Raises each cell to `estimate` (and its lock to `until`), swapping from
    /// exactly what was read; `false` as soon as one swap finds it changed.
    fn raise(
        &self,
        slots: &[usize; DEPTH],
        seen: &[u64; DEPTH],
        cells: &[Cell; DEPTH],
        estimate: u16,
        until: Option<u32>,
    ) -> bool {
        (0..DEPTH).all(|row| {
            let mut next = cells[row];
            next.count = next.count.max(estimate);
            if let Some(until) = until {
                next.until = next.until.max(until);
            }

            let next = next.pack();

            next == seen[row]
                || self.cells[slots[row]]
                    .compare_exchange(seen[row], next, SeqCst, SeqCst)
                    .is_ok()
        })
    }

    /// Zeroes the key's cells: its count and its lock, and any other key's
    /// share of them.
    pub fn clear(&self, hash: u64) {
        for slot in self.slots(hash) {
            self.cells[slot].store(0, SeqCst);
        }
    }

    /// How many of a fixed spread of [`SAMPLE`] cells are locked at `now`, out
    /// of how many were read.
    ///
    /// A sample rather than a scan, so reading it costs the same however full
    /// the table is. The positions are fixed, but nobody outside can aim at
    /// them: which key lands where depends on the hash key.
    pub fn locked_sample(&self, now: Stamp) -> (u32, u32) {
        let total = self.cells.len();
        let sampled = SAMPLE.min(total);
        let stride = total / sampled;
        let locked = (0..sampled)
            .filter(|index| Cell::unpack(self.cells[index * stride].load(SeqCst)).until > now.secs)
            .count();

        (
            u32::try_from(locked).unwrap_or(u32::MAX),
            u32::try_from(sampled).unwrap_or(u32::MAX),
        )
    }

    /// Writes one cell directly, for tests that need a particular state.
    #[cfg(test)]
    pub fn set(&self, hash: u64, row: usize, cell: Cell) {
        self.cells[self.slots(hash)[row]].store(cell.pack(), SeqCst);
    }

    /// Reads the key's cells, for tests.
    #[cfg(test)]
    pub fn cells(&self, hash: u64) -> [Cell; DEPTH] {
        self.read(&self.slots(hash)).map(Cell::unpack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Stamp = Stamp {
        secs: 100,
        window: 1,
    };

    #[test]
    fn a_cell_packs_into_one_word_and_back() {
        let cell = Cell {
            until: 0xDEAD_BEEF,
            count: 0x1234,
            window: 0xFEDC,
        };

        assert_eq!(Cell::unpack(cell.pack()), cell);
        assert_eq!(Cell::default().pack(), 0);
    }

    #[test]
    fn rows_are_separate_and_every_slot_is_inside_its_row() {
        let table = Table::new(8);

        for hash in [0, 1, u64::MAX, 0x0123_4567_89AB_CDEF] {
            for (row, slot) in table.slots(hash).into_iter().enumerate() {
                assert!(
                    (row * 8..row * 8 + 8).contains(&slot),
                    "{hash:x} {row} {slot}"
                );
            }
        }
    }

    #[test]
    fn one_key_alone_counts_exactly() {
        let table = Table::new(64);

        for expected in 1..=20 {
            let recorded = table.record(7, NOW, u16::MAX, 0);

            assert_eq!(recorded.estimate, expected);
            assert_eq!(table.estimate(7, NOW.window), expected);
        }
    }

    #[test]
    fn a_key_is_refused_only_when_every_one_of_its_cells_is_locked() {
        let table = Table::new(64);
        let locked = Cell {
            until: 500,
            ..Cell::default()
        };

        for row in 0..DEPTH - 1 {
            table.set(9, row, locked);
            assert_eq!(table.locked_until(9, NOW), None, "{} of {DEPTH}", row + 1);
        }

        table.set(
            9,
            DEPTH - 1,
            Cell {
                until: 300,
                ..locked
            },
        );
        assert_eq!(
            table.locked_until(9, NOW),
            Some(300),
            "the least lock is the one that counts",
        );
        assert_eq!(
            table.locked_until(
                9,
                Stamp {
                    secs: 300,
                    window: 1
                }
            ),
            None
        );
    }

    #[test]
    fn reaching_the_threshold_locks_every_cell_once() {
        let table = Table::new(64);

        assert_eq!(table.record(3, NOW, 3, 1000).began, None);
        assert_eq!(table.record(3, NOW, 3, 1000).began, None);
        assert_eq!(table.record(3, NOW, 3, 1000).began, Some(1000));
        assert!(table.cells(3).iter().all(|cell| cell.until == 1000));

        let again = table.record(3, NOW, 3, 2000);
        assert_eq!(again.began, None, "a locked key is not locked again");
        assert_eq!(again.estimate, 4, "but its failures still count");
        assert_eq!(table.locked_until(3, NOW), Some(1000), "nor extended");
    }

    #[test]
    fn a_window_that_has_run_out_starts_again_per_cell_and_keeps_its_lock() {
        let table = Table::new(64);

        table.record(5, NOW, 2, 1000);
        table.record(5, NOW, 2, 1000);

        let later = Stamp {
            secs: 200,
            window: NOW.window + 1,
        };
        assert_eq!(table.estimate(5, later.window), 0);
        assert_eq!(table.record(5, later, 2, 2000).estimate, 1);
        assert_eq!(table.locked_until(5, later), Some(1000));
    }

    #[test]
    fn a_cell_a_little_ahead_of_the_writer_is_not_wound_back() {
        let table = Table::new(64);
        let ahead = Stamp {
            secs: 160,
            window: NOW.window + 1,
        };

        table.record(4, ahead, u16::MAX, 0);
        table.record(4, NOW, u16::MAX, 0);

        assert_eq!(table.estimate(4, ahead.window), 2);
        assert!(
            table
                .cells(4)
                .iter()
                .all(|cell| cell.window == ahead.window)
        );
    }

    #[test]
    fn clearing_zeroes_the_count_and_the_lock() {
        let table = Table::new(64);

        table.record(6, NOW, 1, 1000);
        table.clear(6);

        assert_eq!(table.locked_until(6, NOW), None);
        assert_eq!(table.estimate(6, NOW.window), 0);
    }

    #[test]
    fn the_sample_says_how_much_of_the_table_is_locked() {
        let table = Table::new(1024);

        assert_eq!(table.locked_sample(NOW), (0, 4096));

        for hash in 0..10_000u64 {
            table.record(hash.wrapping_mul(0x9E37_79B9_7F4A_7C15), NOW, 1, 1000);
        }

        let (locked, sampled) = table.locked_sample(NOW);
        assert!(locked > sampled / 2, "{locked} of {sampled}");
        assert_eq!(
            table
                .locked_sample(Stamp {
                    secs: 1000,
                    window: 9
                })
                .0,
            0
        );
    }
}
