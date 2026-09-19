//! The channel name → bit position map, kept off the routing path.
//!
//! `<dest group="Blue"/>` names a channel by the string a client typed, and
//! routing needs the bit position behind it. That used to be
//! `db.groups().get_by_name(..).await` — **per `<dest>` element, per message**,
//! against a read pool two connections deep.
//!
//! # What that cost
//!
//! `take_marti` collects every `<dest>` child of every `<marti>`, and an 8 MiB
//! frame holds around a quarter of a million of them. One authenticated device
//! sending one such frame issued 250,000 sequential SQLite reads for the same
//! row, occupying half the read pool for seconds at a time while every `/api/v1`
//! list, every Marti read and every new connection's `resolver::resolve` queued
//! behind it — a full denial of service from one enrolled client, and cheap,
//! because the frame compresses to nothing on the wire (R-03 H2).
//!
//! The cap in [`dest`](super::dest) is the first half of the fix and this is the
//! second: the channel table is small, changes rarely, and is read in full by
//! `resolver` on every connection anyway.
//!
//! # Why it is allowed to be a little stale
//!
//! Channels are administrative: they are created and deleted by a person, and a
//! message that names one is routed against whatever this server last read.
//! Re-reading on a miss is what keeps a newly created channel routable — but
//! re-reading on *every* miss would hand the denial of service straight back,
//! because a miss is exactly what an attacker sends. So a miss re-reads at most
//! once per second, and [`invalidate`](GroupCache::invalidate) is
//! there for the code that knows the table has changed and does not want to
//! wait for it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rustak_core::identity::GroupIndex;

use crate::db::Database;
use crate::prelude::*;

/// How stale the map may be before a miss is allowed to re-read the table.
///
/// One second, so an operator who creates a channel and immediately sends to it
/// is not left wondering, while a client sending names that do not exist costs
/// one read per second however fast it sends them.
const REFRESH_AFTER: Duration = Duration::from_secs(1);

/// The channel index, read from the database and reused.
#[derive(Debug, Default)]
pub struct GroupCache {
    held: RwLock<Option<Cached>>,
    /// Held across the read, so a burst of misses is one read rather than one
    /// each. A `tokio` mutex because it is held over an `.await`.
    reading: tokio::sync::Mutex<()>,
}

/// One reading of the channel table, and when it was taken.
#[derive(Clone, Debug)]
struct Cached {
    index: Arc<GroupIndex>,
    read_at: Instant,
}

impl GroupCache {
    /// An empty cache, which reads the table on its first lookup.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bit position of a named channel, or [`None`] if there is no such
    /// channel.
    ///
    /// A miss re-reads the channel table at most once per second; see the
    /// [module documentation](self).
    pub async fn bitpos(&self, db: &Database, name: &GroupName) -> Option<u32> {
        let held = self.held.read().clone();

        if let Some(held) = &held {
            if let Some(bitpos) = held.index.bitpos(name) {
                return Some(bitpos);
            }

            if held.read_at.elapsed() < REFRESH_AFTER {
                return None;
            }
        }

        let _reading = self.reading.lock().await;

        // Somebody else may have read the table while we waited for the lock,
        // which is the whole point of holding it.
        if let Some(fresh) = self.fresh() {
            return fresh.bitpos(name);
        }

        self.read(db).await?.bitpos(name)
    }

    /// Drops the map, so the next lookup reads the table again.
    ///
    /// For whatever creates, renames or deletes a channel: without it a new
    /// channel is unroutable for up to a second, which is correct but
    /// is a second nobody has to pay for.
    pub fn invalidate(&self) {
        *self.held.write() = None;
    }

    /// The held map, if it is young enough to answer a miss with.
    fn fresh(&self) -> Option<Arc<GroupIndex>> {
        self.held
            .read()
            .as_ref()
            .filter(|held| held.read_at.elapsed() < REFRESH_AFTER)
            .map(|held| Arc::clone(&held.index))
    }

    /// Reads the channel table and keeps what it says.
    async fn read(&self, db: &Database) -> Option<Arc<GroupIndex>> {
        match db.groups().index().await {
            Ok(index) => {
                let index = Arc::new(index);

                *self.held.write() = Some(Cached {
                    index: Arc::clone(&index),
                    read_at: Instant::now(),
                });

                Some(index)
            }
            Err(err) => {
                // Not a reason to close the connection or to answer with a
                // channel the sender does not hold: the message is routed as
                // "no such channel", which is what a database this server
                // cannot read makes true of every channel.
                warn!(error = %err, "Could not read the channel index; a <dest group> will not resolve.");

                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::repos::groups::NewGroup;

    use super::*;

    async fn db_with(names: &[&str]) -> Database {
        let db = Database::open_in_memory().await.unwrap();

        for name in names {
            db.groups()
                .create(NewGroup::manual(GroupName::parse(name).unwrap()))
                .await
                .unwrap();
        }

        db
    }

    #[tokio::test]
    async fn a_channel_resolves_to_its_bit_position() {
        let db = db_with(&["Blue"]).await;
        let cache = GroupCache::new();
        let blue = GroupName::parse("Blue").unwrap();

        let bitpos = cache.bitpos(&db, &blue).await.expect("Blue exists");

        assert_eq!(
            cache.bitpos(&db, &blue).await,
            Some(bitpos),
            "and the second answer is the same one, from the held map",
        );
    }

    #[tokio::test]
    async fn a_channel_that_does_not_exist_resolves_to_nothing() {
        let db = db_with(&["Blue"]).await;
        let cache = GroupCache::new();

        assert_eq!(
            cache
                .bitpos(&db, &GroupName::parse("Nowhere").unwrap())
                .await,
            None,
        );
    }

    #[tokio::test]
    async fn a_flood_of_misses_does_not_become_a_flood_of_reads() {
        // The denial of service this cache exists to close: the version that
        // read the table per `<dest>` element turned one frame into a quarter
        // of a million sequential reads on a two-connection pool.
        let db = db_with(&["Blue"]).await;
        let cache = GroupCache::new();
        let nowhere = GroupName::parse("Nowhere").unwrap();

        // Primes the map, so `read_at` is set.
        assert_eq!(cache.bitpos(&db, &nowhere).await, None);

        let started = Instant::now();
        for _ in 0..100_000 {
            assert_eq!(cache.bitpos(&db, &nowhere).await, None);
        }

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "100,000 misses took {:?}; they are not reaching the database",
            started.elapsed(),
        );
    }

    #[tokio::test]
    async fn a_channel_created_after_the_map_was_read_is_found() {
        let db = db_with(&["Blue"]).await;
        let cache = GroupCache::new();
        let green = GroupName::parse("Green").unwrap();

        assert_eq!(cache.bitpos(&db, &green).await, None, "not there yet");

        db.groups()
            .create(NewGroup::manual(green.clone()))
            .await
            .unwrap();
        cache.invalidate();

        assert!(
            cache.bitpos(&db, &green).await.is_some(),
            "invalidating is what makes a new channel routable at once",
        );
    }
}
