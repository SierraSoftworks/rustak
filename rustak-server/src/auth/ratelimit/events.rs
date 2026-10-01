//! The lockouts that began, newest first, for an administrator to read.
//!
//! A sketch cannot list its keys, so each lockout is also written here as an
//! event when it begins: a ring of [`CAPACITY`] slots, display only. Nothing
//! here is ever consulted for a decision — whether a caller is refused is the
//! sketch's answer alone — so losing an event can hide a lockout from the
//! listing but can never let a caller through or lock one out.
//!
//! # Never waiting on a request path
//!
//! A slot is a mutex, but a writer only ever *tries* it: it takes the next
//! sequence number, and if that slot is busy (an administrator copying it, or
//! another writer a whole ring behind) it takes the next, and after
//! [`TRIES`] busy slots it drops the event and counts it. So recording a
//! lockout is a few atomic operations and a copy, and never blocks. The
//! reader, on the administrator's request, does wait for each slot in turn,
//! one copy at a time.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rustak_api::LockoutClass;

use super::key::{ShownKey, Source};

/// How many lockouts are remembered.
///
/// Above `MAX_LISTED_LOCKOUTS`, so a full listing is possible; small enough
/// that copying the whole ring for one listing is nothing.
pub const CAPACITY: usize = 256;

/// How many busy slots a writer tries before it drops an event.
const TRIES: usize = 4;

/// Which sketch a lockout is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Tier {
    /// Tier 1: the address, at /32 or /64 or /48.
    Source,
    /// Tier 2: the address and the subject.
    Pair,
}

/// A key, with what it takes to find it in a sketch and to describe it.
#[derive(Clone, Copy)]
pub(super) struct Counted {
    pub tier: Tier,
    /// The keyed hash, which is all the sketch needs.
    pub hash: u64,
    pub class: LockoutClass,
    /// The address, cut to the prefix it is counted at.
    pub source: Source,
    /// The folded subject, for a tier-2 key.
    pub subject: Option<ShownKey>,
}

/// One lockout, as it began.
#[derive(Clone, Copy)]
pub(super) struct Event {
    /// Unique and increasing: the order events were recorded in.
    pub seq: u64,
    pub key: Counted,
    pub since: DateTime<Utc>,
    /// The key's estimate when it was locked.
    pub estimate: u16,
    pub cleared: bool,
}

impl Event {
    /// A lockout of `key` that began at `since`, not yet in the ring.
    pub fn new(key: Counted, since: DateTime<Utc>, estimate: u16) -> Self {
        Self {
            seq: 0,
            key,
            since,
            estimate,
            cleared: false,
        }
    }
}

/// The ring.
pub(super) struct Ring {
    slots: Box<[Mutex<Option<Event>>]>,
    next: AtomicU64,
    dropped: AtomicU64,
}

impl Ring {
    pub fn new() -> Self {
        Self {
            slots: (0..CAPACITY).map(|_| Mutex::new(None)).collect(),
            next: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Records `event`, overwriting the oldest; never waits.
    pub fn push(&self, event: Event) {
        for _ in 0..TRIES {
            let seq = self.next.fetch_add(1, Relaxed);
            // `CAPACITY` fits a `u64`, and the remainder fits a `usize`.
            let index = usize::try_from(seq % CAPACITY as u64).unwrap_or_default();

            if let Some(mut slot) = self.slots[index].try_lock() {
                *slot = Some(Event { seq, ..event });
                return;
            }
        }

        self.dropped.fetch_add(1, Relaxed);
    }

    /// Every event held, newest first.
    pub fn newest_first(&self) -> Vec<Event> {
        let mut events: Vec<Event> = self.slots.iter().filter_map(|slot| *slot.lock()).collect();
        events.sort_unstable_by_key(|event| std::cmp::Reverse(event.seq));
        events
    }

    /// Marks every event for this key cleared, and answers the newest of them.
    pub fn clear(&self, tier: Tier, hash: u64) -> Option<Event> {
        let mut newest: Option<Event> = None;

        for slot in &self.slots {
            let mut slot = slot.lock();
            let Some(event) = slot.as_mut() else {
                continue;
            };

            if event.key.tier != tier || event.key.hash != hash || event.cleared {
                continue;
            }

            event.cleared = true;
            if newest.is_none_or(|held| held.seq < event.seq) {
                newest = Some(*event);
            }
        }

        newest
    }

    /// How many events were dropped because every slot tried was busy.
    #[cfg(test)]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }

    /// The bytes the slots occupy, which never changes after [`Ring::new`].
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(&*self.slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(hash: u64) -> Event {
        let key = Counted {
            tier: Tier::Pair,
            hash,
            class: LockoutClass::Account,
            source: Source::host(None),
            subject: Some(ShownKey::of("ada")),
        };

        Event::new(key, Utc::now(), 10)
    }

    #[test]
    fn the_ring_is_bounded_and_newest_first() {
        let ring = Ring::new();
        let bytes = ring.bytes();

        for hash in 0..(CAPACITY as u64 * 3) {
            ring.push(event(hash));
        }

        let held = ring.newest_first();
        assert_eq!(held.len(), CAPACITY);
        assert_eq!(held[0].key.hash, CAPACITY as u64 * 3 - 1);
        assert!(held.windows(2).all(|pair| pair[0].seq > pair[1].seq));
        assert_eq!(ring.bytes(), bytes);
        assert_eq!(ring.dropped(), 0);
    }

    #[test]
    fn a_busy_slot_is_skipped_rather_than_waited_for() {
        let ring = Ring::new();
        let held = ring.slots[0].lock();

        ring.push(event(1));
        drop(held);

        let events = ring.newest_first();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1, "the next slot took it");
    }

    #[test]
    fn a_writer_that_finds_every_slot_busy_drops_the_event_and_says_so() {
        let ring = Ring::new();
        let held: Vec<_> = ring.slots[..TRIES].iter().map(|slot| slot.lock()).collect();

        ring.push(event(1));
        drop(held);

        assert_eq!(ring.dropped(), 1);
        assert!(ring.newest_first().is_empty());
    }

    #[test]
    fn clearing_marks_every_event_for_the_key_and_answers_the_newest() {
        let ring = Ring::new();

        ring.push(event(7));
        ring.push(event(8));
        ring.push(Event {
            estimate: 12,
            ..event(7)
        });

        let cleared = ring.clear(Tier::Pair, 7).unwrap();
        assert_eq!(cleared.estimate, 12);
        assert!(ring.clear(Tier::Pair, 7).is_none(), "already cleared");
        assert!(ring.clear(Tier::Source, 8).is_none(), "another tier");
        assert_eq!(
            ring.newest_first()
                .iter()
                .filter(|event| !event.cleared)
                .count(),
            1
        );
    }
}
