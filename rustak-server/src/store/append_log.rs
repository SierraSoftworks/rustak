//! The append-only segment log that time series are written to.
//!
//! CoT history — and telemetry after it — is a firehose: fifty devices sending
//! a position every two seconds is more writes than the single SQLite writer
//! should ever see, and none of those writes is ever updated afterwards. So
//! they go to files instead. Each stream gets a directory of rolling segment
//! files, each record is a length-prefixed blob (a base-128 varint holding the
//! payload's length, then that many opaque bytes), and SQLite holds only the
//! *index*: which files exist, what time range each covers, how many records
//! and bytes it holds ([`stream_segments`]).
//!
//! That split is what makes the three operations cheap. Appending is a write
//! to the end of a file. Reading a window is an index lookup followed by a
//! sequential scan of a few small files. Retention is `unlink` plus one
//! `DELETE`, rather than a `DELETE` over millions of rows.
//!
//! # The log does not know what it carries
//!
//! A record is opaque bytes the caller has already encoded, and the timestamp
//! passed to [`AppendLog::append`] is used for the *index only* — it is never
//! written into the file. A log that parsed its own records would have to be
//! taught every schema ever written to it, and the payloads rustak stores are
//! already `TakMessage` frames with their own time inside.
//!
//! The consequence is that [`AppendLog::read_range`] resolves to segment
//! granularity: it returns every record of every segment the window touches,
//! and the caller — which can decode a payload — filters precisely.
//!
//! # Crash safety
//!
//! Nothing is fsynced. A record is written and flushed to the operating
//! system, and the index is caught up in batches, so an unclean shutdown can
//! leave a segment ending in a partial frame and an index row that undercounts.
//! [`AppendLog::open`] repairs both: it scans the newest segment, truncates it
//! back to the last complete record, and reconciles the row against what the
//! file actually holds. A row that claims *more* than the file holds means a
//! write was lost, so that segment is sealed and a fresh one started rather
//! than appended to — the index must never point past the end of a file.
//!
//! [`stream_segments`]: crate::db::repos::stream_segments

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::frame::{Frames, MAX_RECORD_BYTES, encode_frame};
use super::segment::{ADVICE_STREAMS, Segment, encode_component, repair};
use crate::db::{Database, repos::StreamSegmentRow};

/// How large a segment grows before the writer rolls to the next one.
///
/// Small enough that a reader parses one quickly and that retention releases
/// space in useful increments; large enough that rolling is rare.
pub const DEFAULT_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

/// How a log rolls its segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendLogOptions {
    /// The size a segment may reach before the next record starts a new file.
    pub max_segment_bytes: u64,
}

impl Default for AppendLogOptions {
    fn default() -> Self {
        Self {
            max_segment_bytes: DEFAULT_SEGMENT_BYTES,
        }
    }
}

/// An append-only log of opaque records for one stream.
pub struct AppendLog {
    db: Database,
    root: PathBuf,
    kind: String,
    key: String,
    /// `<kind>/<key>`, with both components encoded for the filesystem.
    relative: String,
    options: AppendLogOptions,
    current: Option<Segment>,
}

impl std::fmt::Debug for AppendLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppendLog")
            .field("kind", &self.kind)
            .field("key", &self.key)
            .field("segment", &self.current.as_ref().map(Segment::relative))
            .finish()
    }
}

impl AppendLog {
    /// Opens the log for one stream, recovering whatever the last run left.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the stream directory cannot be
    /// created, and a [`human_errors::Kind::System`] error for an unusable
    /// stream key or an index failure.
    #[instrument("store.log.open", skip_all, fields(kind, key), err(Display))]
    pub async fn open(
        db: &Database,
        root: &Path,
        kind: &str,
        key: &str,
        options: AppendLogOptions,
    ) -> Result<Self, Error> {
        let relative = format!("{}/{}", encode_component(kind)?, encode_component(key)?);
        let directory = root.join(&relative);

        tokio::fs::create_dir_all(&directory).await.wrap_user_err(
            format!(
                "We could not create the stream directory '{}'.",
                directory.display()
            ),
            ADVICE_STREAMS,
        )?;

        let mut log = Self {
            db: db.clone(),
            root: root.to_path_buf(),
            kind: kind.to_owned(),
            key: key.to_owned(),
            relative,
            options,
            current: None,
        };

        if let Some(row) = db.stream_segments().open_segment(kind, key).await? {
            log.recover(row).await?;
        }

        Ok(log)
    }

    /// The stream family this log belongs to.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The stream this log records.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The directory this log's segments live in.
    pub fn directory(&self) -> PathBuf {
        self.root.join(&self.relative)
    }

    /// Appends one record, rolling to a new segment when this one is full.
    ///
    /// `time` indexes the record; it is not written to the file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error for a payload larger than a
    /// record may be, and a [`human_errors::Kind::User`] error when the segment
    /// cannot be written.
    pub async fn append(&mut self, time: DateTime<Utc>, payload: &[u8]) -> Result<(), Error> {
        if payload.len() as u64 > MAX_RECORD_BYTES {
            return Err(human_errors::system(
                format!(
                    "A stream record of {} bytes is larger than the {MAX_RECORD_BYTES} byte limit.",
                    payload.len()
                ),
                &[
                    "This is a bug in the caller; please report it with the surrounding log entries.",
                ],
            ));
        }

        let frame = encode_frame(payload);

        if let Some(segment) = &self.current
            && segment.would_overflow(frame.len() as u64, self.options.max_segment_bytes)
        {
            self.seal().await?;
        }

        if self.current.is_none() {
            self.current = Some(
                Segment::create(
                    &self.db,
                    &self.root,
                    &self.kind,
                    &self.key,
                    &self.relative,
                    time,
                )
                .await?,
            );
        }

        let Some(segment) = self.current.as_mut() else {
            return Err(human_errors::system(
                "A stream segment disappeared while a record was being written.",
                &["This is unexpected; please report it with the surrounding log entries."],
            ));
        };

        segment.write(&frame, time).await?;

        if segment.wants_index_flush() {
            self.flush().await?;
        }

        Ok(())
    }

    /// Reports everything appended since the last report to the index.
    ///
    /// Callers writing a batch call this once at the end of it, which keeps
    /// SQLite seeing one write per batch rather than one per record.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the index cannot be written.
    pub async fn flush(&mut self) -> Result<(), Error> {
        let (id, relative, pending) = {
            let Some(segment) = self.current.as_mut() else {
                return Ok(());
            };

            let id = segment.id();
            let Some(pending) = segment.take_pending() else {
                return Ok(());
            };

            (id, segment.relative().to_owned(), pending)
        };

        let (last_time, records, bytes) = pending;

        let indexed = self
            .db
            .stream_segments()
            .record_append(id, last_time, records, bytes)
            .await?;

        if !indexed {
            // The row was sealed or deleted underneath us — by `forget`, by
            // retention, or by the idle sweep. Carrying on would append into a
            // file nothing indexes: the records would be unreadable and, when
            // the file was unlinked, the space would be held until the process
            // exited. So let go of it; the next append starts a fresh segment.
            warn!(
                segment = %relative,
                records,
                "A stream segment's index row is gone; rolling to a new segment."
            );

            self.current = None;
        }

        Ok(())
    }

    /// Finishes the current segment, so the next record starts a new file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the index cannot be written.
    pub async fn seal(&mut self) -> Result<(), Error> {
        self.flush().await?;

        let Some(segment) = self.current.take() else {
            return Ok(());
        };

        self.db.stream_segments().seal(segment.id()).await?;

        debug!(
            segment = %segment.relative(),
            bytes = segment.bytes(),
            "Sealed a stream segment."
        );

        Ok(())
    }

    /// Every record in the segments whose time range touches `[from, to)`.
    ///
    /// Segment-granular by design: the log cannot read a payload's own
    /// timestamp, so it returns whole segments and leaves the precise filter to
    /// the caller. Records are returned oldest segment first, in write order.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when a segment file cannot be read,
    /// and a [`human_errors::Kind::System`] error when the index cannot be
    /// queried.
    #[instrument("store.log.read_range", skip_all, err(Display))]
    pub async fn read_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let mut rows = self
            .db
            .stream_segments()
            .overlapping(&self.kind, &self.key, from, to)
            .await?;

        // The open segment's row lags by up to one flush, so whether it is in
        // the window is decided from what this writer knows rather than from
        // what the row currently says.
        let open = self.current.as_ref().filter(|segment| {
            rows.retain(|row| row.id != segment.id());

            segment.touches(from, to)
        });

        let mut records = Vec::new();

        for row in rows {
            self.read_segment(&row.segment_path, &mut records).await?;
        }

        if let Some(segment) = open {
            self.read_segment(segment.relative(), &mut records).await?;
        }

        Ok(records)
    }

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
        let expired = db.stream_segments().expired_before(before).await?;
        let removed = Self::remove_indexed(db, root, expired).await?;

        if removed > 0 {
            info!(segments = removed, "Pruned expired stream segments.");
        }

        Ok(removed)
    }

    /// Unlinks each segment's file and forgets its row, reporting how many went.
    ///
    /// The file goes first — a row without its file is recoverable, a file
    /// without its row is invisible and leaks.
    ///
    /// Public because retention has a second list to sweep — the segments a
    /// stream keeps past `[retention] cot_history_max_rows`, which the index
    /// picks out ([`over_row_cap`]) and `cot_store::retention` hands back here.
    ///
    /// [`over_row_cap`]: crate::db::repos::StreamSegmentsRepo::over_row_cap
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

        for row in rows {
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

            // Best effort, and it fails harmlessly while the stream still has
            // segments: an empty directory per retired device would otherwise
            // accumulate forever.
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::remove_dir(parent).await;
            }

            db.stream_segments().delete(row.id).await?;
            removed += 1;
        }

        Ok(removed)
    }

    /// Reconciles an index row left open by the previous run with its file.
    async fn recover(&mut self, row: StreamSegmentRow) -> Result<(), Error> {
        // The common case is not a crash: it is a log that was parked to make
        // room in the writer's cache, whose file is exactly as long as its row
        // says because parking flushes. One `stat` settles that, where the scan
        // below reads up to `max_segment_bytes` — which at fleet scale would be
        // the entire cost of a cache miss.
        if let Ok(metadata) = tokio::fs::metadata(self.root.join(&row.segment_path)).await
            && metadata.len() == row.byte_length
        {
            if row.byte_length >= self.options.max_segment_bytes {
                self.db.stream_segments().seal(row.id).await?;
                return Ok(());
            }

            let bytes = row.byte_length;

            self.current = Some(Segment::adopt(&self.root, row, bytes).await?);

            return Ok(());
        }

        let Some(scan) = repair(&self.root.join(&row.segment_path), &row.segment_path).await?
        else {
            warn!(
                segment = %row.segment_path,
                "A stream segment's file is missing; sealing its index row and starting a new segment."
            );
            self.db.stream_segments().seal(row.id).await?;
            return Ok(());
        };

        if scan.records < row.record_count || scan.bytes < row.byte_length {
            warn!(
                segment = %row.segment_path,
                indexed_records = row.record_count,
                found_records = scan.records,
                "A stream segment holds less than its index claims; sealing it rather than appending."
            );
            self.db.stream_segments().seal(row.id).await?;
            return Ok(());
        }

        if scan.records > row.record_count || scan.bytes > row.byte_length {
            // Records that reached the file but never reached the index. The
            // timestamp cannot be recovered per record, so the row keeps the
            // last one it was told about; the next append moves it forward.
            self.db
                .stream_segments()
                .record_append(
                    row.id,
                    row.last_time,
                    scan.records - row.record_count,
                    scan.bytes - row.byte_length,
                )
                .await?;
        }

        if scan.bytes >= self.options.max_segment_bytes {
            self.db.stream_segments().seal(row.id).await?;
            return Ok(());
        }

        self.current = Some(Segment::adopt(&self.root, row, scan.bytes).await?);

        Ok(())
    }

    /// Appends every record of one segment file to `records`.
    async fn read_segment(&self, relative: &str, records: &mut Vec<Vec<u8>>) -> Result<(), Error> {
        let path = self.root.join(relative);

        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                warn!(segment = %relative, "A stream segment is indexed but its file is gone.");
                return Ok(());
            }
            Err(err) => {
                return Err(err).wrap_user_err(
                    format!("We could not read the stream segment '{relative}'."),
                    ADVICE_STREAMS,
                );
            }
        };

        records.extend(Frames::new(&bytes).map(<[u8]>::to_vec));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIND: &str = "cot";
    const KEY: &str = "ANDROID-1";

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-18T10:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::TimeDelta::seconds(seconds)
    }

    fn tiny() -> AppendLogOptions {
        AppendLogOptions {
            max_segment_bytes: 64,
        }
    }

    async fn log(db: &Database, root: &Path, options: AppendLogOptions) -> AppendLog {
        AppendLog::open(db, root, KIND, KEY, options).await.unwrap()
    }

    #[tokio::test]
    async fn a_segment_whose_row_was_deleted_rolls_instead_of_appending_into_a_hole() {
        // `cot_store::query::forget` unlinks a segment the writer may still
        // hold open. Carrying on would write up to a whole segment's worth of
        // records into an unreachable inode, holding the space until the
        // process exited. The log notices at its next flush and rolls.
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;

        log.append(at(0), b"one").await.unwrap();
        log.flush().await.unwrap();

        let rows = db
            .stream_segments()
            .overlapping(KIND, KEY, at(-60), at(60))
            .await
            .unwrap();
        let orphaned = rows[0].segment_path.clone();

        db.stream_segments().delete(rows[0].id).await.unwrap();

        log.append(at(1), b"two").await.unwrap();
        log.flush().await.unwrap();
        log.append(at(2), b"three").await.unwrap();
        log.flush().await.unwrap();

        let rows = db
            .stream_segments()
            .overlapping(KIND, KEY, at(-60), at(60))
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].segment_path, orphaned);
        assert_eq!(rows[0].record_count, 1);
    }

    #[tokio::test]
    async fn a_stream_directory_retention_removed_is_recreated_on_the_next_append() {
        // `remove_indexed` removes a stream's directory with its last segment.
        // A log opened before that sweep used to fail with `ENOENT` for the
        // rest of the process's life, because only `open` created directories.
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;

        tokio::fs::remove_dir_all(log.directory()).await.unwrap();

        log.append(at(0), b"one").await.unwrap();
        log.flush().await.unwrap();

        assert_eq!(
            log.read_range(at(-60), at(60)).await.unwrap(),
            vec![b"one".to_vec()]
        );
    }

    #[tokio::test]
    async fn records_read_back_in_write_order() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;

        for (index, payload) in [&b"one"[..], b"", b"three"].into_iter().enumerate() {
            log.append(at(index as i64), payload).await.unwrap();
        }
        log.flush().await.unwrap();

        assert_eq!(
            log.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"one".to_vec(), Vec::new(), b"three".to_vec()]
        );
    }

    #[tokio::test]
    async fn a_reopened_log_carries_on_in_the_same_segment() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        let mut first = log(&db, dir.path(), AppendLogOptions::default()).await;
        first.append(at(0), b"before").await.unwrap();
        first.flush().await.unwrap();
        drop(first);

        let mut second = log(&db, dir.path(), AppendLogOptions::default()).await;
        second.append(at(1), b"after").await.unwrap();
        second.flush().await.unwrap();

        assert_eq!(
            second.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"before".to_vec(), b"after".to_vec()]
        );
        assert_eq!(
            db.stream_segments()
                .overlapping(KIND, KEY, at(-1), at(60))
                .await
                .unwrap()
                .len(),
            1,
            "reopening must not start a second segment"
        );
    }

    #[tokio::test]
    async fn a_partial_frame_left_by_a_crash_is_truncated_on_open() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        let mut first = log(&db, dir.path(), AppendLogOptions::default()).await;
        first.append(at(0), b"complete").await.unwrap();
        first.flush().await.unwrap();
        let path = dir.path().join(
            db.stream_segments()
                .open_segment(KIND, KEY)
                .await
                .unwrap()
                .unwrap()
                .segment_path,
        );
        drop(first);

        // The shape a crash mid-write leaves: a length prefix promising nine
        // bytes and only four of them on disk.
        let mut torn = tokio::fs::read(&path).await.unwrap();
        torn.extend_from_slice(&[9, b't', b'o', b'r', b'n']);
        tokio::fs::write(&path, &torn).await.unwrap();

        let mut recovered = log(&db, dir.path(), AppendLogOptions::default()).await;

        assert_eq!(
            tokio::fs::metadata(&path).await.unwrap().len(),
            9,
            "the file is truncated back to the complete record"
        );
        assert_eq!(
            recovered.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"complete".to_vec()]
        );

        recovered.append(at(1), b"next").await.unwrap();
        recovered.flush().await.unwrap();
        assert_eq!(
            recovered.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"complete".to_vec(), b"next".to_vec()],
            "appending resumes cleanly after the repair"
        );
    }

    #[tokio::test]
    async fn records_that_never_reached_the_index_are_counted_on_open() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        // No flush: the file has the record, the index row does not know.
        let mut first = log(&db, dir.path(), AppendLogOptions::default()).await;
        first.append(at(0), b"unindexed").await.unwrap();
        let row = db
            .stream_segments()
            .open_segment(KIND, KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.record_count, 0);
        drop(first);

        log(&db, dir.path(), AppendLogOptions::default()).await;

        let repaired = db
            .stream_segments()
            .open_segment(KIND, KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repaired.record_count, 1);
        assert_eq!(repaired.byte_length, 10);
    }

    #[tokio::test]
    async fn an_index_row_claiming_more_than_its_file_is_sealed_rather_than_appended_to() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        let mut first = log(&db, dir.path(), AppendLogOptions::default()).await;
        first.append(at(0), b"kept").await.unwrap();
        first.flush().await.unwrap();
        let row = db
            .stream_segments()
            .open_segment(KIND, KEY)
            .await
            .unwrap()
            .unwrap();
        drop(first);

        // A write the index was told about but that never reached the disk.
        db.stream_segments()
            .record_append(row.id, at(1), 5, 500)
            .await
            .unwrap();

        let mut recovered = log(&db, dir.path(), AppendLogOptions::default()).await;
        recovered.append(at(2), b"fresh").await.unwrap();
        recovered.flush().await.unwrap();

        let open = db
            .stream_segments()
            .open_segment(KIND, KEY)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            open.id, row.id,
            "the suspect segment was sealed, not appended to"
        );
        assert_eq!(
            recovered.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"kept".to_vec(), b"fresh".to_vec()],
            "both segments are still readable"
        );
    }

    #[tokio::test]
    async fn a_missing_segment_file_does_not_stop_the_log() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        let mut first = log(&db, dir.path(), AppendLogOptions::default()).await;
        first.append(at(0), b"gone").await.unwrap();
        first.flush().await.unwrap();
        let row = db
            .stream_segments()
            .open_segment(KIND, KEY)
            .await
            .unwrap()
            .unwrap();
        drop(first);

        tokio::fs::remove_file(dir.path().join(&row.segment_path))
            .await
            .unwrap();

        let mut recovered = log(&db, dir.path(), AppendLogOptions::default()).await;
        recovered.append(at(1), b"new").await.unwrap();
        recovered.flush().await.unwrap();

        assert_eq!(
            recovered.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"new".to_vec()]
        );
    }

    #[tokio::test]
    async fn a_full_segment_rolls_to_the_next_file() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), tiny()).await;

        for index in 0..10 {
            log.append(at(index), &[b'x'; 20]).await.unwrap();
        }
        log.flush().await.unwrap();

        let segments = db
            .stream_segments()
            .overlapping(KIND, KEY, at(-1), at(60))
            .await
            .unwrap();

        assert!(
            segments.len() >= 3,
            "ten 21-byte records cannot fit in 64-byte segments"
        );
        assert!(
            segments
                .iter()
                .all(|row| row.byte_length <= tiny().max_segment_bytes),
            "no segment may exceed the roll size"
        );
        assert_eq!(log.read_range(at(-1), at(60)).await.unwrap().len(), 10);
    }

    #[tokio::test]
    async fn a_record_larger_than_a_segment_still_gets_its_own_file() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), tiny()).await;

        log.append(at(0), &[b'y'; 500]).await.unwrap();
        log.flush().await.unwrap();

        assert_eq!(
            log.read_range(at(-1), at(60)).await.unwrap(),
            vec![vec![b'y'; 500]]
        );
    }

    #[tokio::test]
    async fn a_window_reads_only_the_segments_it_touches() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), tiny()).await;

        for index in 0..10 {
            log.append(at(index * 10), &[b'0' + index as u8; 20])
                .await
                .unwrap();
        }
        log.seal().await.unwrap();

        let all = log.read_range(at(-1), at(1_000)).await.unwrap();
        let window = log.read_range(at(0), at(15)).await.unwrap();

        assert_eq!(all.len(), 10);
        assert!(
            window.len() < all.len(),
            "a narrow window reads fewer segments"
        );
        assert!(
            window.contains(&vec![b'0'; 20]),
            "the first record is in the window"
        );
        assert!(
            log.read_range(at(10_000), at(20_000))
                .await
                .unwrap()
                .is_empty(),
            "a window past every segment reads nothing"
        );
    }

    #[tokio::test]
    async fn the_unflushed_tail_of_the_open_segment_is_still_readable() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;

        // No flush: the index row still says the segment ends at its creation
        // time, so only the writer's own view can place it in the window.
        log.append(at(30), b"recent").await.unwrap();

        assert_eq!(
            log.read_range(at(20), at(40)).await.unwrap(),
            vec![b"recent".to_vec()]
        );
    }

    #[tokio::test]
    async fn pruning_unlinks_sealed_segments_and_forgets_their_rows() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), tiny()).await;

        for index in 0..6 {
            log.append(at(index * 10), &[b'z'; 20]).await.unwrap();
        }
        log.seal().await.unwrap();

        let before_prune = db
            .stream_segments()
            .overlapping(KIND, KEY, at(-1), at(1_000))
            .await
            .unwrap();
        let pruned = AppendLog::prune_before(&db, dir.path(), at(25))
            .await
            .unwrap();

        assert!(pruned > 0, "the oldest segments are expired");
        let after = db
            .stream_segments()
            .overlapping(KIND, KEY, at(-1), at(1_000))
            .await
            .unwrap();
        assert_eq!(after.len(), before_prune.len() - pruned);

        for row in &after {
            assert!(
                tokio::fs::try_exists(dir.path().join(&row.segment_path))
                    .await
                    .unwrap(),
                "a surviving row keeps its file"
            );
        }
        assert_eq!(
            AppendLog::prune_before(&db, dir.path(), at(25))
                .await
                .unwrap(),
            0,
            "pruning twice removes nothing the second time"
        );
    }

    #[tokio::test]
    async fn pruning_leaves_the_segment_still_being_written() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;
        log.append(at(0), b"open").await.unwrap();
        log.flush().await.unwrap();

        assert_eq!(
            AppendLog::prune_before(&db, dir.path(), at(1_000))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            log.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"open".to_vec()]
        );
    }

    #[tokio::test]
    async fn two_streams_keep_separate_directories() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        let mut first = AppendLog::open(
            &db,
            dir.path(),
            KIND,
            "ANDROID-1",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        let mut second = AppendLog::open(
            &db,
            dir.path(),
            KIND,
            "ANDROID-2",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();

        first.append(at(0), b"first").await.unwrap();
        second.append(at(0), b"second").await.unwrap();
        first.flush().await.unwrap();
        second.flush().await.unwrap();

        assert_eq!(
            first.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"first".to_vec()]
        );
        assert_eq!(
            second.read_range(at(-1), at(60)).await.unwrap(),
            vec![b"second".to_vec()]
        );
        assert_ne!(first.directory(), second.directory());
    }

    #[tokio::test]
    async fn a_hostile_stream_key_cannot_escape_its_directory() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();

        let log = AppendLog::open(
            &db,
            dir.path(),
            KIND,
            "../../etc",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();

        assert!(
            log.directory().starts_with(dir.path().join(KIND)),
            "{} escaped the stream root",
            log.directory().display()
        );
        assert!(
            AppendLog::open(&db, dir.path(), KIND, "", AppendLogOptions::default())
                .await
                .is_err()
        );
        assert!(
            AppendLog::open(
                &db,
                dir.path(),
                KIND,
                &"x".repeat(81),
                AppendLogOptions::default()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_record_larger_than_the_frame_limit_is_refused() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;

        let too_big = vec![0u8; MAX_RECORD_BYTES as usize + 1];

        assert!(log.append(at(0), &too_big).await.is_err());
    }

    #[tokio::test]
    #[ignore = "throughput sanity check; run with --ignored"]
    async fn a_hundred_thousand_appends_are_fast_enough() {
        let db = database().await;
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(&db, dir.path(), AppendLogOptions::default()).await;

        let payload = [b'p'; 180];
        let started = std::time::Instant::now();

        for index in 0..100_000i64 {
            log.append(at(index / 100), &payload).await.unwrap();
        }
        log.flush().await.unwrap();

        let elapsed = started.elapsed();
        eprintln!("100k appends in {elapsed:?}");

        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "100k appends took {elapsed:?}, which is far past a sane budget"
        );
        assert_eq!(
            log.read_range(at(-1), at(100_000)).await.unwrap().len(),
            100_000
        );
    }
}
