//! CoT history, as append-only segment files rather than database rows.
//!
//! One [`AppendLog`] per uid, under the `cot` stream family, holding the
//! protobuf payload of every message that uid sent. SQLite keeps only the
//! segment index, which is what makes retention an `unlink` and a `DELETE`
//! rather than a delete over millions of rows.
//!
//! # Why the open logs are capped, and why the cap is large
//!
//! A log holds an open file handle and a partly written segment, so a
//! thousand devices would otherwise hold a thousand of both and a process
//! file-descriptor limit is a bad way to find that out. But the cap has to sit
//! *above* the number of devices that report concurrently, because the access
//! pattern of a live fleet — every uid in turn, every couple of seconds — is
//! the pathological case for a least-recently-used cache. A cap below the
//! fleet size means every single message misses, and a miss used to cost a
//! seal, a fresh file and three SQLite writes. Hence
//! `[storage] open_history_logs`, defaulting to [`DEFAULT_OPEN_LOGS`], which is
//! an order of magnitude above the connection ceiling the design commits to.
//!
//! # Why eviction does not seal
//!
//! Sealing an evicted log is what made a miss expensive: the index row is
//! closed, so the next append finds nothing to continue and creates a brand-new
//! segment file — one file per message, per device, for ever. Eviction
//! therefore only *parks* the log: whatever it has appended is reported to the
//! index and the file handle is dropped, while the row stays open. Reopening
//! adopts that row and appends to the same file, so a device that cycles
//! through the cache costs one index read rather than a new file.
//!
//! A parked row is left unsealed, and retention only prunes sealed segments, so
//! [`sweep`] seals rows nothing has appended to for a while. Sealing one out
//! from under a live writer is safe: [`AppendLog::flush`] notices that its row
//! is gone and rolls to a fresh segment.
//!
//! [`sweep`]: super::retention::sweep

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::prelude::*;
use crate::store::append_log::{AppendLog, AppendLogOptions};

use super::STREAM_KIND;

/// How many per-uid logs are held open at once by default.
///
/// `[storage] open_history_logs` overrides it.
pub const DEFAULT_OPEN_LOGS: usize = 4096;

/// One open log, and where it sits in the eviction order.
#[derive(Debug)]
struct Held {
    log: AppendLog,
    /// This log's key in [`HistoryWriter::order`].
    seq: u64,
}

/// The per-uid append logs one writer task owns.
#[derive(Debug)]
pub struct HistoryWriter {
    db: Database,
    root: PathBuf,
    options: AppendLogOptions,
    max_open: usize,
    logs: HashMap<String, Held>,
    /// Least-recently-used first; the eviction order.
    ///
    /// A map rather than a `Vec` so that marking a log used is a pair of
    /// logarithmic operations rather than a scan of every open log — at the
    /// fleet sizes the cap is now sized for, the scan was itself the cost.
    order: BTreeMap<u64, String>,
    /// The next eviction-order key; monotonic, so it also orders ties.
    next_seq: u64,
    /// How many logs have been parked to make room, for the operator.
    evictions: u64,
}

impl HistoryWriter {
    /// Prepares to write history under `root`.
    pub fn new(db: Database, root: impl Into<PathBuf>) -> Self {
        Self {
            db,
            root: root.into(),
            options: AppendLogOptions::default(),
            max_open: DEFAULT_OPEN_LOGS,
            logs: HashMap::new(),
            order: BTreeMap::new(),
            next_seq: 0,
            evictions: 0,
        }
    }

    /// Replaces the segment-rolling options.
    #[must_use]
    pub fn with_options(mut self, options: AppendLogOptions) -> Self {
        self.options = options;
        self
    }

    /// Replaces the open-log cap.
    #[must_use]
    pub fn with_max_open(mut self, max_open: usize) -> Self {
        self.max_open = max_open.max(1);
        self
    }

    /// Where the segments live.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Appends one message to the log for `uid`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the segment cannot be written
    /// and a [`human_errors::Kind::System`] error when the index cannot be
    /// updated.
    pub async fn append(
        &mut self,
        uid: &str,
        time: DateTime<Utc>,
        payload: &[u8],
    ) -> Result<(), Error> {
        self.touch(uid).await?;

        let Some(held) = self.logs.get_mut(uid) else {
            return Ok(());
        };

        held.log.append(time, payload).await
    }

    /// Flushes every open log, so the index catches up with the files.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the index cannot be updated.
    pub async fn flush(&mut self) -> Result<(), Error> {
        for held in self.logs.values_mut() {
            held.log.flush().await?;
        }

        Ok(())
    }

    /// Flushes and closes everything, for a clean shutdown.
    ///
    /// Sealing rather than only flushing: a segment left open is one retention
    /// will not delete, and a process that has stopped has no reason to hold
    /// any of them open.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the index cannot be updated.
    pub async fn close(&mut self) -> Result<(), Error> {
        for (uid, mut held) in self.logs.drain() {
            if let Err(err) = held.log.seal().await {
                warn!(uid = %uid, error = %err, "Could not seal a CoT history segment on the way out.");
            }
        }

        self.order.clear();

        Ok(())
    }

    /// How many logs are open.
    pub fn open_logs(&self) -> usize {
        self.logs.len()
    }

    /// How many logs have been parked to make room for another.
    ///
    /// A number that climbs with the message rate means the cap is below the
    /// fleet size and every message is paying for a reopen.
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Opens the log for `uid` if it is not open, and marks it most recently
    /// used.
    async fn touch(&mut self, uid: &str) -> Result<(), Error> {
        let seq = self.next_seq;
        self.next_seq += 1;

        if let Some(held) = self.logs.get_mut(uid) {
            let previous = std::mem::replace(&mut held.seq, seq);

            self.order.remove(&previous);
            self.order.insert(seq, uid.to_owned());

            return Ok(());
        }

        while self.logs.len() >= self.max_open {
            let Some((_, evicted)) = self.order.pop_first() else {
                break;
            };

            self.park(&evicted).await;
        }

        let log = AppendLog::open(&self.db, &self.root, STREAM_KIND, uid, self.options).await?;

        self.logs.insert(uid.to_owned(), Held { log, seq });
        self.order.insert(seq, uid.to_owned());

        Ok(())
    }

    /// Reports an evicted log's appends to the index and lets go of its file.
    ///
    /// Deliberately not a seal: the row stays open so that the next append for
    /// this uid continues the same segment rather than starting a new one.
    async fn park(&mut self, uid: &str) {
        let Some(mut held) = self.logs.remove(uid) else {
            return;
        };

        self.evictions += 1;

        // Once per power of two: a cap below the fleet size makes this the
        // hottest path in the writer, and the operator's fix is one setting.
        if self.evictions.is_power_of_two() && self.evictions >= 1024 {
            warn!(
                evictions = self.evictions,
                open_logs = self.max_open,
                "CoT history logs are being parked to make room; consider raising [storage] open_history_logs."
            );
        }

        if let Err(err) = held.log.flush().await {
            warn!(uid = %uid, error = %err, "Could not flush an idle CoT history segment.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn writer(root: &Path) -> (Database, HistoryWriter) {
        let db = Database::open_in_memory().await.unwrap();

        (db.clone(), HistoryWriter::new(db, root))
    }

    /// Every segment file under `root`, however deep.
    fn segment_files(root: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(root) else {
            return 0;
        };

        entries
            .filter_map(Result::ok)
            .map(|entry| {
                if entry.path().is_dir() {
                    segment_files(&entry.path())
                } else {
                    1
                }
            })
            .sum()
    }

    #[tokio::test]
    async fn a_message_is_readable_back_out_of_its_segment() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut writer) = writer(dir.path()).await;
        let now = Utc::now();

        writer.append("UID-A", now, b"first").await.unwrap();
        writer.append("UID-A", now, b"second").await.unwrap();
        writer.flush().await.unwrap();

        let log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();

        let records = log
            .read_range(
                now - chrono::Duration::hours(1),
                now + chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert_eq!(records, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[tokio::test]
    async fn each_uid_gets_its_own_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (_db, mut writer) = writer(dir.path()).await;
        let now = Utc::now();

        writer.append("UID-A", now, b"a").await.unwrap();
        writer.append("UID-B", now, b"b").await.unwrap();
        writer.flush().await.unwrap();

        assert_eq!(writer.open_logs(), 2);
    }

    #[tokio::test]
    async fn the_open_logs_are_capped_and_the_idlest_is_closed_first() {
        // A thousand devices must not mean a thousand open file handles.
        let dir = tempfile::tempdir().unwrap();
        let (_db, writer) = writer(dir.path()).await;
        let mut writer = writer.with_max_open(2);
        let now = Utc::now();

        writer.append("UID-A", now, b"a").await.unwrap();
        writer.append("UID-B", now, b"b").await.unwrap();
        writer.append("UID-A", now, b"a2").await.unwrap();
        writer.append("UID-C", now, b"c").await.unwrap();

        assert_eq!(writer.open_logs(), 2);

        writer.close().await.unwrap();
        assert_eq!(writer.open_logs(), 0);
    }

    #[tokio::test]
    async fn an_evicted_stream_is_appended_to_again_rather_than_lost() {
        let dir = tempfile::tempdir().unwrap();
        let (db, writer) = writer(dir.path()).await;
        let mut writer = writer.with_max_open(1);
        let now = Utc::now();

        writer.append("UID-A", now, b"first").await.unwrap();
        writer.append("UID-B", now, b"other").await.unwrap();
        writer.append("UID-A", now, b"second").await.unwrap();
        writer.close().await.unwrap();

        let log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();

        let records = log
            .read_range(
                now - chrono::Duration::hours(1),
                now + chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert_eq!(records, vec![b"first".to_vec(), b"second".to_vec()]);
        // And both records are in the *same* file: eviction parks the log, it
        // does not seal it, so the reopen continued the segment it left.
        assert_eq!(segment_files(&log.directory()), 1);
    }

    #[tokio::test]
    async fn round_robin_access_costs_one_segment_per_device_not_one_per_message() {
        // The shape of C2. A fleet reporting in turn is the worst case for a
        // least-recently-used cache, and every miss used to seal the evicted
        // log — so the next message for that device created a whole new file.
        // Eighty messages over eight devices meant eighty segment files.
        let dir = tempfile::tempdir().unwrap();
        let (db, writer) = writer(dir.path()).await;
        let mut writer = writer.with_max_open(2);
        let now = Utc::now();
        let uids = [
            "UID-A", "UID-B", "UID-C", "UID-D", "UID-E", "UID-F", "UID-G", "UID-H",
        ];

        for round in 0..10i64 {
            for uid in uids {
                writer
                    .append(uid, now + chrono::Duration::milliseconds(round), b"x")
                    .await
                    .unwrap();
            }
        }

        writer.close().await.unwrap();

        let segments = segment_files(dir.path());

        assert!(
            segments <= uids.len() * 2,
            "{segments} segment files for {} devices; the writer is thrashing",
            uids.len()
        );
        assert_eq!(segments, uids.len());

        // And nothing was lost on the way: every round is still readable.
        let log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();

        let records = log
            .read_range(
                now - chrono::Duration::hours(1),
                now + chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert_eq!(records.len(), 10);
    }

    #[tokio::test]
    async fn a_parked_log_is_reopened_without_rereading_its_segment() {
        // The cheap-miss half of C2: the reopen trusts the flushed index when
        // the file is exactly as long as the row says, rather than scanning up
        // to a full segment on every eviction cycle.
        let dir = tempfile::tempdir().unwrap();
        let (db, writer) = writer(dir.path()).await;
        let mut writer = writer.with_max_open(1);
        let now = Utc::now();

        writer.append("UID-A", now, b"first").await.unwrap();
        writer.append("UID-B", now, b"other").await.unwrap();
        writer.append("UID-A", now, b"second").await.unwrap();
        writer.flush().await.unwrap();

        let open = db
            .stream_segments()
            .open_segment(STREAM_KIND, "UID-A")
            .await
            .unwrap()
            .expect("the parked segment's row stays open");

        assert_eq!(open.record_count, 2);
        assert_eq!(writer.evictions(), 2);
    }
}
