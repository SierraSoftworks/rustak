//! One segment file: what it is called, how it is written, how it is repaired.
//!
//! [`AppendLog`](super::AppendLog) decides *which* segment to write to and when
//! to roll; everything about an individual file lives here. The split keeps the
//! log's own logic readable, and it puts the two pieces of this design that are
//! easy to get quietly wrong — the filesystem-safe naming and the reconciliation
//! of a file against its index row — in one place with their own tests.

use std::path::Path;

use chrono::{DateTime, Utc};
use rand::Rng as _;
use rustak_core::prelude::*;
use tokio::io::AsyncWriteExt as _;

use super::frame::Frames;
use crate::db::{
    Database,
    repos::{NewStreamSegment, StreamSegmentRow},
};

/// The extension every segment file carries.
const SEGMENT_SUFFIX: &str = ".log";

/// How much randomness distinguishes two segments opened in the same
/// millisecond.
const SEGMENT_NONCE_BYTES: usize = 4;

/// The longest stream key we will turn into a directory name.
///
/// Percent-encoding can triple a key's length, so 80 bytes is the largest key
/// that always fits inside the 255-byte component limit every filesystem we
/// support imposes. TAK unique identifiers are well under half of that.
const MAX_STREAM_KEY_BYTES: usize = 80;

/// How many records may be appended before the index row is caught up.
const INDEX_FLUSH_RECORDS: u64 = 256;

/// How many bytes may be appended before the index row is caught up.
const INDEX_FLUSH_BYTES: u64 = 256 * 1024;

/// Advice for a segment-file failure the operator can act on.
pub(super) const ADVICE_STREAMS: &[&str] = &[
    "Check that the directory in [storage] streams_dir is writable by the user rustak runs as.",
    "Check that the filesystem holding it has free space and free inodes.",
];

/// How many segment files this process has created.
///
/// A rate that tracks the message rate means the history writer is thrashing:
/// a healthy installation creates one segment per device per roll, not one per
/// message. Worth an alert.
static SEGMENTS_CREATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many segment files this process has created since it started.
pub fn segments_created() -> u64 {
    SEGMENTS_CREATED.load(std::sync::atomic::Ordering::Relaxed)
}

/// What a scan of a segment file actually found in it.
pub(super) struct Scan {
    /// Complete records.
    pub(super) records: u64,
    /// Bytes up to the end of the last complete record.
    pub(super) bytes: u64,
}

/// Reads a segment file and truncates any partial record at its end.
///
/// `Ok(None)` means the file is not there — the state a segment is left in when
/// it was unlinked out from under its index row.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the file is there but cannot be
/// truncated.
pub(super) async fn repair(path: &Path, relative: &str) -> Result<Option<Scan>, Error> {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return Ok(None);
    };

    let mut frames = Frames::new(&bytes);
    let records = frames.by_ref().count() as u64;
    let valid = frames.valid_len();

    if valid < bytes.len() as u64 {
        warn!(
            segment = %relative,
            discarded = bytes.len() as u64 - valid,
            "Truncating a stream segment back to its last complete record."
        );

        let failed = || format!("We could not repair the stream segment '{relative}'.");

        tokio::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .await
            .wrap_user_err(failed(), ADVICE_STREAMS)?
            .set_len(valid)
            .await
            .wrap_user_err(failed(), ADVICE_STREAMS)?;
    }

    Ok(Some(Scan {
        records,
        bytes: valid,
    }))
}

/// The segment a log is currently appending to.
pub(super) struct Segment {
    /// The `stream_segments` row that indexes it.
    id: i64,
    /// Its path relative to the stream root, as the index stores it.
    relative: String,
    file: tokio::fs::File,
    /// Bytes in the file, including everything not yet reported to the index.
    bytes: u64,
    first_time: DateTime<Utc>,
    last_time: DateTime<Utc>,
    /// Records appended since the index was last caught up.
    pending_records: u64,
    /// Bytes appended since the index was last caught up.
    pending_bytes: u64,
}

impl Segment {
    /// Creates the next segment file and indexes it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the file cannot be created,
    /// and a [`human_errors::Kind::System`] error when it cannot be indexed.
    pub(super) async fn create(
        db: &Database,
        root: &Path,
        kind: &str,
        key: &str,
        directory: &str,
        first_time: DateTime<Utc>,
    ) -> Result<Self, Error> {
        let relative = format!("{directory}/{}", segment_name(first_time));
        let path = root.join(&relative);

        // The directory is created here rather than only in `AppendLog::open`:
        // retention removes a stream's directory once its last segment goes, so
        // a log that was opened before that sweep and appends after it would
        // otherwise fail with `ENOENT` for the life of the process.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.wrap_user_err(
                format!(
                    "We could not create the stream directory '{}'.",
                    parent.display()
                ),
                ADVICE_STREAMS,
            )?;
        }

        // The index row first, and the file second. A row whose file is missing
        // is a state everything here already handles — `repair` reports it,
        // `recover` seals it, retention unlinks nothing and deletes the row —
        // whereas a file with no row is invisible to every one of them and
        // leaks for the life of the volume.
        let row = db
            .stream_segments()
            .create(NewStreamSegment {
                stream_kind: kind.to_owned(),
                stream_key: key.to_owned(),
                segment_path: relative.clone(),
                first_time,
            })
            .await?;

        let created = tokio::fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .await
            .wrap_user_err(
                format!(
                    "We could not create the stream segment '{}'.",
                    path.display()
                ),
                ADVICE_STREAMS,
            );

        let file = match created {
            Ok(file) => file,
            Err(err) => {
                let _ = db.stream_segments().delete(row.id).await;
                return Err(err);
            }
        };

        SEGMENTS_CREATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(Self {
            id: row.id,
            relative,
            file,
            bytes: 0,
            first_time,
            last_time: first_time,
            pending_records: 0,
            pending_bytes: 0,
        })
    }

    /// Reopens a repaired segment so the log can carry on appending to it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the file cannot be reopened.
    pub(super) async fn adopt(
        root: &Path,
        row: StreamSegmentRow,
        bytes: u64,
    ) -> Result<Self, Error> {
        let file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(root.join(&row.segment_path))
            .await
            .wrap_user_err(
                format!(
                    "We could not reopen the stream segment '{}'.",
                    row.segment_path
                ),
                ADVICE_STREAMS,
            )?;

        Ok(Self {
            id: row.id,
            relative: row.segment_path,
            file,
            bytes,
            first_time: row.first_time,
            last_time: row.last_time,
            pending_records: 0,
            pending_bytes: 0,
        })
    }

    /// The `stream_segments` row that indexes this file.
    pub(super) fn id(&self) -> i64 {
        self.id
    }

    /// This file's path relative to the stream root.
    pub(super) fn relative(&self) -> &str {
        &self.relative
    }

    /// How large the file is, including anything not yet indexed.
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Whether adding `length` bytes would take the file past `limit`.
    ///
    /// An empty segment never overflows: a record larger than the roll size
    /// has to go somewhere, and rolling before writing it would loop forever.
    pub(super) fn would_overflow(&self, length: u64, limit: u64) -> bool {
        self.bytes > 0 && self.bytes + length > limit
    }

    /// Whether this segment holds anything a query for `[from, to)` wants.
    ///
    /// Answered from what the writer knows rather than from the index row,
    /// which lags by up to one flush.
    pub(super) fn touches(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
        self.first_time < to && self.last_time >= from
    }

    /// Writes one encoded record to the end of the file.
    ///
    /// Flushed to the operating system but not fsynced: a lost tail is what
    /// [`repair`] exists to trim, and an fsync per record would cost far more
    /// than it buys.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the write fails.
    pub(super) async fn write(&mut self, frame: &[u8], time: DateTime<Utc>) -> Result<(), Error> {
        let failed = || {
            format!(
                "We could not append to the stream segment '{}'.",
                self.relative
            )
        };

        self.file
            .write_all(frame)
            .await
            .wrap_user_err(failed(), ADVICE_STREAMS)?;
        self.file
            .flush()
            .await
            .wrap_user_err(failed(), ADVICE_STREAMS)?;

        let length = frame.len() as u64;

        self.bytes += length;
        self.pending_bytes += length;
        self.pending_records += 1;
        self.last_time = self.last_time.max(time);

        Ok(())
    }

    /// Whether enough has accumulated to be worth an index write.
    pub(super) fn wants_index_flush(&self) -> bool {
        self.pending_records >= INDEX_FLUSH_RECORDS || self.pending_bytes >= INDEX_FLUSH_BYTES
    }

    /// Takes what has not been reported to the index yet, if anything has.
    pub(super) fn take_pending(&mut self) -> Option<(DateTime<Utc>, u64, u64)> {
        if self.pending_records == 0 && self.pending_bytes == 0 {
            return None;
        }

        let pending = (self.last_time, self.pending_records, self.pending_bytes);

        self.pending_records = 0;
        self.pending_bytes = 0;

        Some(pending)
    }
}

/// Names a segment file after the time it opens, plus a little randomness.
///
/// Naming by time rather than by a sequence number means no name has to be
/// derived from what is already on disk — which is exactly what breaks after a
/// segment file has been lost, because the number it used is still spoken for
/// in the index and reusing it collides. A listing still sorts in write order,
/// and an operator can see at a glance which file holds which hour.
fn segment_name(opened: DateTime<Utc>) -> String {
    let mut nonce = [0u8; SEGMENT_NONCE_BYTES];
    rand::rng().fill_bytes(&mut nonce);

    format!(
        "{}-{}{SEGMENT_SUFFIX}",
        opened.format("%Y%m%dT%H%M%S%.3fZ"),
        hex::encode(nonce)
    )
}

/// Renders one path component for a stream kind or key.
///
/// Anything outside `[A-Za-z0-9._-]` becomes `%XX`, and a leading dot is
/// escaped too, so no key can name `.`, `..` or anything outside its own
/// directory however the client that supplied it chose it. The encoding is
/// reversible, which keeps a directory listing readable for an operator looking
/// for one device's history.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error for a key that is empty or longer
/// than a path component may be.
pub(super) fn encode_component(value: &str) -> Result<String, Error> {
    if value.is_empty() || value.len() > MAX_STREAM_KEY_BYTES {
        return Err(human_errors::system(
            format!("A stream key of {} bytes cannot be stored.", value.len()),
            &[
                "A stream key must be between 1 and 80 bytes; please report this with the surrounding log entries.",
            ],
        ));
    }

    let mut encoded = String::with_capacity(value.len());

    for (index, byte) in value.bytes().enumerate() {
        if byte.is_ascii_alphanumeric()
            || byte == b'-'
            || byte == b'_'
            || (byte == b'.' && index > 0)
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(hex_digit(byte >> 4)));
            encoded.push(char::from(hex_digit(byte & 0x0f)));
        }
    }

    Ok(encoded)
}

/// One uppercase hexadecimal digit.
fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'A' + (nibble - 10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-18T10:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::TimeDelta::seconds(seconds)
    }

    #[test]
    fn a_key_is_encoded_reversibly_and_never_as_a_traversal() {
        assert_eq!(encode_component("ANDROID-1").unwrap(), "ANDROID-1");
        assert_eq!(encode_component("a.b").unwrap(), "a.b");
        assert_eq!(encode_component(".").unwrap(), "%2E");
        assert_eq!(encode_component("..").unwrap(), "%2E.");
        assert_eq!(encode_component("a/b").unwrap(), "a%2Fb");
        assert_eq!(encode_component("a\\b").unwrap(), "a%5Cb");
        assert_eq!(encode_component("a b").unwrap(), "a%20b");
        assert_eq!(encode_component("café").unwrap(), "caf%C3%A9");
    }

    #[test]
    fn an_encoded_key_always_fits_a_path_component() {
        let worst = encode_component(&" ".repeat(MAX_STREAM_KEY_BYTES)).unwrap();

        assert_eq!(worst.len(), MAX_STREAM_KEY_BYTES * 3);
        assert!(worst.len() < 255, "every filesystem we support allows 255");
    }

    #[test]
    fn an_unusable_key_is_refused_rather_than_mangled() {
        assert!(encode_component("").is_err());
        assert!(encode_component(&"x".repeat(MAX_STREAM_KEY_BYTES + 1)).is_err());
        assert!(encode_component(&"x".repeat(MAX_STREAM_KEY_BYTES)).is_ok());
    }

    #[test]
    fn segment_names_sort_in_write_order_and_never_repeat() {
        let first = segment_name(at(0));
        let second = segment_name(at(60));

        assert!(first < second, "{first} should sort before {second}");
        assert!(first.starts_with("20260918T100000.000Z-"), "{first}");
        assert!(first.ends_with(SEGMENT_SUFFIX));
        assert_ne!(
            segment_name(at(0)),
            segment_name(at(0)),
            "two segments opened in the same millisecond must not collide"
        );
    }

    #[tokio::test]
    async fn repairing_a_file_that_is_not_there_reports_so() {
        let dir = tempfile::tempdir().unwrap();

        assert!(
            repair(&dir.path().join("gone.log"), "gone.log")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn repairing_counts_whole_records_and_trims_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0.log");

        let mut bytes = super::super::frame::encode_frame(b"one");
        bytes.extend_from_slice(&super::super::frame::encode_frame(b"two"));
        bytes.extend_from_slice(&[9, b'p', b'a', b'r', b't']);
        tokio::fs::write(&path, &bytes).await.unwrap();

        let scan = repair(&path, "0.log").await.unwrap().unwrap();

        assert_eq!(scan.records, 2);
        assert_eq!(scan.bytes, 8);
        assert_eq!(tokio::fs::metadata(&path).await.unwrap().len(), 8);

        // Repairing a file that is already whole changes nothing.
        let again = repair(&path, "0.log").await.unwrap().unwrap();
        assert_eq!(again.records, 2);
        assert_eq!(again.bytes, 8);
    }

    #[tokio::test]
    async fn a_segment_accumulates_until_it_is_asked_for_its_pending_counts() {
        let db = Database::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("cot/A"))
            .await
            .unwrap();

        let mut segment = Segment::create(&db, dir.path(), "cot", "A", "cot/A", at(0))
            .await
            .unwrap();

        assert!(segment.take_pending().is_none(), "nothing written yet");

        segment.write(b"\x03abc", at(5)).await.unwrap();
        assert!(
            !segment.wants_index_flush(),
            "one small record is not worth a write"
        );
        assert_eq!(segment.bytes(), 4);

        assert_eq!(segment.take_pending(), Some((at(5), 1, 4)));
        assert!(
            segment.take_pending().is_none(),
            "taking twice reports nothing"
        );
    }

    #[tokio::test]
    async fn a_segment_knows_when_it_is_full_and_what_window_it_covers() {
        let db = Database::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("cot/A"))
            .await
            .unwrap();

        let mut segment = Segment::create(&db, dir.path(), "cot", "A", "cot/A", at(0))
            .await
            .unwrap();

        assert!(
            !segment.would_overflow(1_000, 10),
            "an empty segment takes a record larger than the roll size"
        );

        segment.write(b"\x03abc", at(10)).await.unwrap();

        assert!(segment.would_overflow(100, 10));
        assert!(!segment.would_overflow(1, 10));
        assert!(segment.touches(at(-5), at(20)));
        assert!(
            segment.touches(at(10), at(20)),
            "the window starts on its last record"
        );
        assert!(!segment.touches(at(11), at(20)));
        assert!(
            !segment.touches(at(-20), at(0)),
            "the window ends on its first record"
        );
    }
}
