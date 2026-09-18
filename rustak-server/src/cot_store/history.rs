//! CoT history, as append-only segment files rather than database rows.
//!
//! One [`AppendLog`] per uid, under the `cot` stream family, holding the
//! protobuf payload of every message that uid sent. SQLite keeps only the
//! segment index, which is what makes retention an `unlink` and a `DELETE`
//! rather than a delete over millions of rows.
//!
//! # Why the open logs are capped
//!
//! A log holds an open file handle and a partly written segment. An
//! installation with a thousand devices would otherwise hold a thousand of
//! both, and a process file-descriptor limit is a bad way to find that out. The
//! writer keeps the [`MAX_OPEN_LOGS`] most recently used and seals the rest —
//! sealing is cheap, and a device that has gone quiet is exactly the one whose
//! segment should be closed and made available to retention.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::prelude::*;
use crate::store::append_log::{AppendLog, AppendLogOptions};

use super::STREAM_KIND;

/// How many per-uid logs are held open at once.
pub const MAX_OPEN_LOGS: usize = 256;

/// The per-uid append logs one writer task owns.
#[derive(Debug)]
pub struct HistoryWriter {
    db: Database,
    root: PathBuf,
    options: AppendLogOptions,
    max_open: usize,
    logs: HashMap<String, AppendLog>,
    /// Least-recently-used first; the eviction order.
    order: Vec<String>,
}

impl HistoryWriter {
    /// Prepares to write history under `root`.
    pub fn new(db: Database, root: impl Into<PathBuf>) -> Self {
        Self {
            db,
            root: root.into(),
            options: AppendLogOptions::default(),
            max_open: MAX_OPEN_LOGS,
            logs: HashMap::new(),
            order: Vec::new(),
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

        let Some(log) = self.logs.get_mut(uid) else {
            return Ok(());
        };

        log.append(time, payload).await
    }

    /// Flushes every open log, so the index catches up with the files.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the index cannot be updated.
    pub async fn flush(&mut self) -> Result<(), Error> {
        for log in self.logs.values_mut() {
            log.flush().await?;
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
        for (uid, mut log) in self.logs.drain() {
            if let Err(err) = log.seal().await {
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

    /// Opens the log for `uid` if it is not open, and marks it most recently
    /// used.
    async fn touch(&mut self, uid: &str) -> Result<(), Error> {
        if self.logs.contains_key(uid) {
            self.order.retain(|held| held != uid);
            self.order.push(uid.to_owned());

            return Ok(());
        }

        while self.logs.len() >= self.max_open {
            let Some(evicted) = (!self.order.is_empty()).then(|| self.order.remove(0)) else {
                break;
            };

            if let Some(mut log) = self.logs.remove(&evicted)
                && let Err(err) = log.seal().await
            {
                warn!(uid = %evicted, error = %err, "Could not seal an idle CoT history segment.");
            }
        }

        let log = AppendLog::open(&self.db, &self.root, STREAM_KIND, uid, self.options).await?;

        self.logs.insert(uid.to_owned(), log);
        self.order.push(uid.to_owned());

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn writer(root: &Path) -> (Database, HistoryWriter) {
        let db = Database::open_in_memory().await.unwrap();

        (db.clone(), HistoryWriter::new(db, root))
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
    }
}
