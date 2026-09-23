//! Retiring segments: the half of [`AppendLog`] that retention drives.
//!
//! Apart from the log itself because it is a different concern with a
//! different shape: nothing here holds a log open. It works from index rows —
//! whichever ones a caller's query picked out — and its cost is bounded in both
//! directions. Rows are **read a page at a time** ([`PRUNE_PAGE`]), because the
//! first sweep of an installation that is far past a limit it has only just
//! been given can have millions of surplus segments, and their paths do not
//! all need to be in memory to be unlinked. And they are **forgotten a batch
//! per transaction** ([`REMOVE_BATCH`]), because the single SQLite writer has
//! other callers.

use std::path::Path;

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::AppendLog;
use crate::db::{Database, repos::StreamSegmentRow};

/// How many surplus index rows are read, and so held in memory, at once.
///
/// A few megabytes of rows, and the whole of what a sweep holds: the reads
/// behind a page walk an index in order rather than sorting it
/// (`db::repos::stream_segments::retention`), so nothing else grows with the
/// size of the index. Large rather than small because each cap read opens with
/// an aggregate over the index, which a page should be worth.
pub const PRUNE_PAGE: usize = 10_000;

/// How many segments [`AppendLog::remove_indexed`] forgets per transaction.
const REMOVE_BATCH: usize = 500;

/// Which index rows a removal is after; each is one retention limit.
///
/// A value rather than a closure handing back pages: a sweep runs inside a job
/// whose future has to be `Send`, which a closure borrowing the database does
/// not yet manage to promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surplus<'a> {
    /// Sealed segments of any kind whose newest record predates the instant.
    ExpiredBefore(DateTime<Utc>),
    /// Sealed segments of a stream kind past a per-stream-key record cap.
    OverRowCap(&'a str, u64),
    /// Sealed segments of a stream kind past a cap on the kind's total bytes.
    OverByteCap(&'a str, u64),
}

impl Surplus<'_> {
    /// The first `limit` rows this still finds, oldest first.
    async fn page(self, db: &Database, limit: usize) -> Result<Vec<StreamSegmentRow>, Error> {
        let segments = db.stream_segments();

        match self {
            Self::ExpiredBefore(before) => segments.expired_before(before, limit).await,
            Self::OverRowCap(kind, rows) => segments.over_row_cap(kind, rows, limit).await,
            Self::OverByteCap(kind, bytes) => segments.over_byte_cap(kind, bytes, limit).await,
        }
    }
}

impl AppendLog {
    /// Deletes every sealed segment whose newest record predates `before`.
    ///
    /// Sweeps every stream rather than just one, because that is what the
    /// retention job wants: one pass over the index, unlinking files and
    /// forgetting rows. The file goes first — a row without its file is
    /// recoverable, a file without its row is invisible and leaks.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the index cannot be read or
    /// written. A file that cannot be unlinked is logged and left indexed, so
    /// the next sweep tries again.
    #[instrument("store.log.prune", skip_all, err(Display))]
    pub async fn prune_before(
        db: &Database,
        root: &Path,
        before: DateTime<Utc>,
    ) -> Result<usize, Error> {
        let removed =
            Self::remove_paged(db, root, Surplus::ExpiredBefore(before), PRUNE_PAGE).await?;

        if removed > 0 {
            info!(segments = removed, "Pruned expired stream segments.");
        }

        Ok(removed)
    }

    /// Removes everything `surplus` finds, at most `page_size` rows at a time.
    ///
    /// Each round asks for the *first* `page_size` surplus rows, not for an
    /// offset: what the previous round removed is no longer surplus,
    /// so the query's own answer moves forward.
    ///
    /// Which is also how this could fail to end — a segment whose file cannot
    /// be unlinked stays indexed and comes back on the next page. So a round
    /// that removes nothing is the last: the rows left are the ones that will
    /// not go, and the next sweep tries them again.
    ///
    /// # Errors
    ///
    /// As [`remove_indexed`](Self::remove_indexed), or when the index cannot be
    /// read.
    pub async fn remove_paged(
        db: &Database,
        root: &Path,
        surplus: Surplus<'_>,
        page_size: usize,
    ) -> Result<usize, Error> {
        let mut removed = 0;

        loop {
            let rows = surplus.page(db, page_size).await?;
            let last = rows.len() < page_size;
            let gone = Self::remove_indexed(db, root, rows).await?;
            removed += gone;

            if last || gone == 0 {
                return Ok(removed);
            }
        }
    }

    /// Unlinks each segment's file and forgets its row, reporting how many went.
    ///
    /// The file goes first — a row without its file is recoverable, a file
    /// without its row is invisible and leaks.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when a row cannot be deleted. A
    /// file that cannot be unlinked is logged and left indexed, so the next
    /// sweep tries again.
    pub async fn remove_indexed(
        db: &Database,
        root: &Path,
        rows: Vec<StreamSegmentRow>,
    ) -> Result<usize, Error> {
        let mut removed = 0;

        for batch in rows.chunks(REMOVE_BATCH) {
            let mut gone = Vec::with_capacity(batch.len());

            for row in batch {
                let path = root.join(&row.segment_path);

                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        warn!(
                            segment = %row.segment_path,
                            error = %err,
                            "Could not remove a stream segment; it will be retried."
                        );
                        continue;
                    }
                }

                // Best effort, and it fails harmlessly while the stream still
                // has segments: an empty directory per retired device would
                // otherwise accumulate forever.
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::remove_dir(parent).await;
                }

                gone.push(row.id);
            }

            removed += db.stream_segments().delete_many(gone).await?;
        }

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewStreamSegment;

    /// A sealed, indexed segment an hour old, with whatever is at its path.
    async fn sealed_row(db: &Database, path: &str) {
        let row = db
            .stream_segments()
            .create(NewStreamSegment {
                stream_kind: "cot".into(),
                stream_key: path.into(),
                segment_path: path.into(),
                first_time: Utc::now() - chrono::Duration::hours(1),
            })
            .await
            .unwrap();
        db.stream_segments().seal(row.id).await.unwrap();
    }

    async fn everything(db: &Database) -> Vec<StreamSegmentRow> {
        db.stream_segments()
            .expired_before(Utc::now(), 100)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn more_segments_than_a_page_holds_all_go() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        for index in 0..5 {
            sealed_row(&db, &format!("cot/uid-{index}/0001.log")).await;
        }

        let removed =
            AppendLog::remove_paged(&db, dir.path(), Surplus::ExpiredBefore(Utc::now()), 2)
                .await
                .unwrap();

        assert_eq!(removed, 5);
        assert!(everything(&db).await.is_empty());
    }

    #[tokio::test]
    async fn a_segment_that_will_not_go_does_not_keep_the_sweep_going_for_ever() {
        // A directory where the file should be: `unlink` refuses it, so the row
        // stays indexed and is the first thing every later page finds.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        sealed_row(&db, "cot/stuck/0001.log").await;
        std::fs::create_dir_all(dir.path().join("cot/stuck/0001.log/occupied")).unwrap();

        let removed =
            AppendLog::remove_paged(&db, dir.path(), Surplus::ExpiredBefore(Utc::now()), 1)
                .await
                .unwrap();

        assert_eq!(removed, 0);
        assert_eq!(
            everything(&db).await.len(),
            1,
            "left indexed, to be retried"
        );
    }
}
