//! Keeping the CoT history inside `[retention]`.
//!
//! Two sweeps with different costs. The history is whole segment files, so
//! pruning it is an `unlink` and a `DELETE` per file rather than a delete over
//! millions of rows — which is why the effective horizon is the configured age
//! *plus the tail of the segment that spans it*, and why that is documented
//! rather than worked around. `cot_latest` is one row per uid and is swept by
//! staleness instead: a device that has not been heard from since long past its
//! own stale time is one nothing should still be drawing on a map.

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::prelude::*;
use crate::store::append_log::AppendLog;

use super::latest;

/// What one sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// History segment files deleted.
    pub segments: usize,
    /// `cot_latest` rows deleted.
    pub latest: usize,
}

impl Swept {
    /// Whether the sweep found anything at all.
    pub fn is_empty(self) -> bool {
        self.segments == 0 && self.latest == 0
    }
}

/// Removes history older than `before`, and rows stale since before it.
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
) -> Result<Swept, Error> {
    let segments = AppendLog::prune_before(db, streams_dir, before).await?;
    let latest = latest::prune_stale(db, before).await?;

    let swept = Swept { segments, latest };

    if !swept.is_empty() {
        info!(
            segments,
            rows = latest,
            "Removed CoT history past its retention."
        );
    }

    Ok(swept)
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

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7))
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

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7))
            .await
            .unwrap();

        assert_eq!(swept.segments, 0);
    }

    #[tokio::test]
    async fn a_sweep_with_nothing_to_do_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();

        let swept = sweep(&db, dir.path(), Utc::now() - chrono::Duration::days(7))
            .await
            .unwrap();

        assert!(swept.is_empty());
    }
}
