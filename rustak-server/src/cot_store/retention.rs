//! Keeping the CoT history inside `[retention]`.
//!
//! Three sweeps with different costs. The history is whole segment files, so
//! pruning it is an `unlink` and a `DELETE` per file rather than a delete over
//! millions of rows — which is why the effective horizon is the configured age
//! *plus the tail of the segment that spans it*, and why that is documented
//! rather than worked around. `cot_latest` is one row per uid and is swept by
//! staleness instead: a device that has not been heard from since long past its
//! own stale time is one nothing should still be drawing on a map.
//!
//! # Why an age horizon is not enough on its own
//!
//! `[retention] cot_history` alone lets a busy installation fill the disk
//! *inside* the window: a hundred devices reporting ten times a second for a
//! week is not a quantity the configured seven days says anything about. So
//! `[retention] cot_history_max_rows` is a second, independent floor, and both
//! are enforced — which is what `config.example.toml` has always claimed.
//!
//! The cap is **per stream key**, which for CoT is per device uid, so that one
//! talkative source cannot evict everybody else's history. Like the age
//! horizon it is approximate in the keeping direction: whole segments are
//! deleted, so a stream keeps the cap plus the tail of the segment that
//! straddles it. Deciding which segments are surplus is a window function over
//! the index ([`StreamSegmentsRepo::over_row_cap`]) rather than a scan in
//! Rust — the row counts are already a column, and adding them up here would
//! mean reading every segment of every stream into memory.
//!
//! [`StreamSegmentsRepo::over_row_cap`]: crate::db::repos::StreamSegmentsRepo::over_row_cap

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::prelude::*;
use crate::store::append_log::AppendLog;

use super::{STREAM_KIND, latest};

/// What one sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// History segment files deleted because they were older than the horizon.
    pub segments: usize,
    /// History segment files deleted because their stream was over its cap.
    pub over_cap: usize,
    /// `cot_latest` rows deleted.
    pub latest: usize,
}

impl Swept {
    /// Whether the sweep found anything at all.
    pub fn is_empty(self) -> bool {
        self.segments == 0 && self.over_cap == 0 && self.latest == 0
    }
}

/// Removes history older than `before` or past `max_rows`, and stale rows.
///
/// `max_rows` is `[retention] cot_history_max_rows`, per stream key. Zero means
/// the cap is not enforced rather than *keep nothing*: history is files on
/// disk, and an installation that wants none of it turns `[stream.limits]
/// record_history` off instead of asking for every sealed segment to be
/// deleted six hours after it was written.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the index cannot be read or
/// written. A segment file that cannot be unlinked is logged and left indexed,
/// so the next sweep tries again.
#[instrument("cot_store.retention.sweep", skip_all, err(Display))]
pub async fn sweep(
    db: &Database,
    streams_dir: &std::path::Path,
    before: DateTime<Utc>,
    max_rows: u64,
) -> Result<Swept, Error> {
    // Age first: a segment past the horizon is gone whatever the counts say,
    // and removing it is one fewer row for the cap's window function to add up.
    let segments = AppendLog::prune_before(db, streams_dir, before).await?;
    let over_cap = prune_over_cap(db, streams_dir, max_rows).await?;
    let latest = latest::prune_stale(db, before).await?;

    let swept = Swept {
        segments,
        over_cap,
        latest,
    };

    if !swept.is_empty() {
        info!(
            segments,
            over_cap,
            rows = latest,
            "Removed CoT history past its retention."
        );
    }

    Ok(swept)
}

/// Deletes the segments each stream keeps past `max_rows` records.
async fn prune_over_cap(
    db: &Database,
    streams_dir: &std::path::Path,
    max_rows: u64,
) -> Result<usize, Error> {
    if max_rows == 0 {
        return Ok(0);
    }

    let surplus = db
        .stream_segments()
        .over_row_cap(STREAM_KIND, max_rows)
        .await?;

    if surplus.is_empty() {
        return Ok(0);
    }

    AppendLog::remove_indexed(db, streams_dir, surplus).await
}

#[cfg(test)]
mod tests {
    use super::super::STREAM_KIND;
    use super::*;
    use crate::store::append_log::AppendLogOptions;

    #[tokio::test]
    async fn a_sealed_segment_older_than_the_horizon_goes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let old = Utc::now() - chrono::Duration::days(30);

        let mut log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        log.append(old, b"ancient").await.unwrap();
        log.seal().await.unwrap();

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 0)
            .await
            .unwrap();

        assert_eq!(swept.segments, 1);
    }

    #[tokio::test]
    async fn an_open_segment_is_left_alone() {
        // The writer is still appending to it; unlinking it would take the
        // history of whatever is happening right now.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let old = Utc::now() - chrono::Duration::days(30);

        let mut log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        log.append(old, b"ancient").await.unwrap();
        log.flush().await.unwrap();

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 0)
            .await
            .unwrap();

        assert_eq!(swept.segments, 0);
    }

    /// A stream with `segments` sealed one-record segments, oldest first.
    async fn sealed_segments(db: &Database, dir: &std::path::Path, key: &str, segments: usize) {
        for index in 0..segments {
            let mut log = AppendLog::open(
                db,
                dir,
                STREAM_KIND,
                key,
                AppendLogOptions {
                    // One record per segment, so "records" and "segments" are
                    // the same number and the arithmetic under test is visible.
                    max_segment_bytes: 1,
                },
            )
            .await
            .unwrap();

            log.append(
                Utc::now() - chrono::Duration::minutes((segments - index) as i64),
                b"x",
            )
            .await
            .unwrap();
            log.seal().await.unwrap();
        }
    }

    async fn segment_count(db: &Database, key: &str) -> usize {
        db.stream_segments()
            .overlapping(
                STREAM_KIND,
                key,
                Utc::now() - chrono::Duration::days(1),
                Utc::now() + chrono::Duration::days(1),
            )
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn a_stream_over_the_row_cap_loses_its_oldest_segments() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        sealed_segments(&db, dir.path(), "UID-A", 5).await;

        // Two records' worth kept, so the three oldest segments are surplus:
        // the newest two alone already reach the cap.
        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 2)
            .await
            .unwrap();

        assert_eq!(swept.segments, 0, "nothing is past the age horizon");
        assert_eq!(swept.over_cap, 3);
        assert_eq!(segment_count(&db, "UID-A").await, 2);
    }

    #[tokio::test]
    async fn the_cap_is_per_stream_so_one_busy_device_evicts_only_itself() {
        // The whole point of a per-key cap: a device reporting ten times a
        // second must not take the quiet device beside it down with it.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        sealed_segments(&db, dir.path(), "UID-BUSY", 6).await;
        sealed_segments(&db, dir.path(), "UID-QUIET", 2).await;

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 3)
            .await
            .unwrap();

        assert_eq!(swept.over_cap, 3);
        assert_eq!(segment_count(&db, "UID-BUSY").await, 3);
        assert_eq!(
            segment_count(&db, "UID-QUIET").await,
            2,
            "the quiet stream is under the cap and keeps everything",
        );
    }

    #[tokio::test]
    async fn a_stream_inside_the_cap_keeps_every_segment() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        sealed_segments(&db, dir.path(), "UID-A", 3).await;

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 100)
            .await
            .unwrap();

        assert!(swept.is_empty());
        assert_eq!(segment_count(&db, "UID-A").await, 3);
    }

    #[tokio::test]
    async fn the_segment_being_written_survives_the_cap() {
        // Unsealed means "the present". It still counts towards the cap — it is
        // the newest thing there is — but it is never the thing deleted.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        sealed_segments(&db, dir.path(), "UID-A", 3).await;

        let mut open = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        open.append(Utc::now(), b"now").await.unwrap();
        open.flush().await.unwrap();

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 1)
            .await
            .unwrap();

        assert_eq!(swept.over_cap, 3, "the three sealed segments are surplus");
        assert_eq!(segment_count(&db, "UID-A").await, 1);
    }

    #[tokio::test]
    async fn a_cap_of_zero_is_no_cap_at_all() {
        // Not "keep nothing": an installation that wants no history turns
        // `[stream.limits] record_history` off.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        sealed_segments(&db, dir.path(), "UID-A", 4).await;

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 0)
            .await
            .unwrap();

        assert!(swept.is_empty());
        assert_eq!(segment_count(&db, "UID-A").await, 4);
    }

    #[tokio::test]
    async fn a_sweep_with_nothing_to_do_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7), 0)
            .await
            .unwrap();

        assert!(swept.is_empty());
    }
}
