//! `stream_segments`: the index over the append-only segment files.
//!
//! Time series — CoT history now, telemetry later — are not stored in SQLite.
//! They are written to `store::append_log` segment files, one directory per
//! stream key, each record a varint-length-prefixed protobuf frame. Only the
//! *metadata* is here: where each segment is, what time range it covers, how
//! many records and bytes it holds, and whether the writer has finished with it.
//!
//! That split is the reason there is no `cot_history` table. A busy
//! installation writes thousands of positions a second; putting those in SQLite
//! would make every web read wait behind them. Reading back means finding the
//! segments whose range overlaps the query here, then seeking in the files;
//! retention means deleting whole segments, which is one unlink and one row.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    row::{Timestamp, bool_col, ts},
};

/// The columns [`StreamSegmentRow::from_row`] expects, in order.
const COLUMNS: &str = "id, stream_kind, stream_key, segment_path, first_time, last_time, \
                       record_count, byte_length, sealed, created_at";

/// A page size as SQLite binds it.
///
/// The three retention queries below all take one: what they find is about to
/// be deleted, a first sweep can find millions, and the caller asks again
/// until a page comes back short ([`AppendLog::remove_paged`]).
///
/// [`AppendLog::remove_paged`]: crate::store::AppendLog::remove_paged
fn page_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// One row of `stream_segments`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSegmentRow {
    pub id: i64,
    /// Which stream family: `cot` today.
    pub stream_kind: String,
    /// What keys it within that family: a CoT uid, say.
    pub stream_key: String,
    /// Where the file is, relative to the stream root.
    pub segment_path: String,
    pub first_time: DateTime<Utc>,
    pub last_time: DateTime<Utc>,
    pub record_count: u64,
    pub byte_length: u64,
    /// `false` while the writer is still appending.
    pub sealed: bool,
    pub created_at: DateTime<Utc>,
}

impl StreamSegmentRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            stream_kind: row.get(1)?,
            stream_key: row.get(2)?,
            segment_path: row.get(3)?,
            first_time: ts(row, 4)?,
            last_time: ts(row, 5)?,
            record_count: row.get::<_, i64>(6)?.max(0) as u64,
            byte_length: row.get::<_, i64>(7)?.max(0) as u64,
            sealed: bool_col(row, 8)?,
            created_at: ts(row, 9)?,
        })
    }
}

/// A segment the writer has just opened.
#[derive(Debug, Clone)]
pub struct NewStreamSegment {
    pub stream_kind: String,
    pub stream_key: String,
    pub segment_path: String,
    /// The time of the first record, which is also where the index starts.
    pub first_time: DateTime<Utc>,
}

/// Reads and writes `stream_segments`.
pub struct StreamSegmentsRepo<'a> {
    db: &'a Database,
}

impl<'a> StreamSegmentsRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Records a newly opened segment.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when that path is
    /// already indexed — two writers appending to one file would interleave
    /// their frames.
    pub async fn create(&self, new: NewStreamSegment) -> Result<StreamSegmentRow, Error> {
        self.db
            .write(move |tx| {
                let first = Timestamp::from(new.first_time);

                tx.query_one(
                    &format!(
                        "INSERT INTO stream_segments \
                           (stream_kind, stream_key, segment_path, first_time, last_time, \
                            created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?4, ?5) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.stream_kind,
                        new.stream_key,
                        new.segment_path,
                        first,
                        Timestamp::now(),
                    ],
                    StreamSegmentRow::from_row,
                )
            })
            .await
    }

    /// Records what the writer has appended since the last update.
    ///
    /// Counts are added rather than set, so a writer only has to report its
    /// own progress and two updates cannot lose each other's records.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn record_append(
        &self,
        id: i64,
        last_time: DateTime<Utc>,
        records: u64,
        bytes: u64,
    ) -> Result<bool, Error> {
        let last_time = Timestamp::from(last_time);

        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE stream_segments \
                     SET last_time = MAX(last_time, ?2), \
                         record_count = record_count + ?3, \
                         byte_length = byte_length + ?4 \
                     WHERE id = ?1 AND sealed = 0",
                    rusqlite::params![id, last_time, records as i64, bytes as i64],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Closes a segment: the writer has moved on to the next file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn seal(&self, id: i64) -> Result<bool, Error> {
        let sealed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE stream_segments SET sealed = 1 WHERE id = ?1 AND sealed = 0",
                    [id],
                )
            })
            .await?;

        Ok(sealed > 0)
    }

    /// The segment a writer should carry on appending to, if there is one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn open_segment(
        &self,
        stream_kind: &str,
        stream_key: &str,
    ) -> Result<Option<StreamSegmentRow>, Error> {
        let (stream_kind, stream_key) = (stream_kind.to_owned(), stream_key.to_owned());

        self.db
            .read(move |c| {
                c.query_one(
                    &format!(
                        "SELECT {COLUMNS} FROM stream_segments \
                         WHERE stream_kind = ?1 AND stream_key = ?2 AND sealed = 0 \
                         ORDER BY first_time DESC LIMIT 1"
                    ),
                    [stream_kind, stream_key],
                    StreamSegmentRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// The segments whose time range overlaps `[from, to)`, oldest first.
    ///
    /// The overlap test is `first_time < to AND last_time >= from`, which keeps
    /// a segment whose first record is before the window but whose last is
    /// inside it — the common case when a query starts partway through a file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn overlapping(
        &self,
        stream_kind: &str,
        stream_key: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<StreamSegmentRow>, Error> {
        let (stream_kind, stream_key) = (stream_kind.to_owned(), stream_key.to_owned());
        let (from, to) = (Timestamp::from(from), Timestamp::from(to));

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM stream_segments \
                     WHERE stream_kind = ?1 AND stream_key = ?2 \
                       AND first_time < ?4 AND last_time >= ?3 \
                     ORDER BY first_time ASC"
                ))?;
                let rows = statement.query_map(
                    rusqlite::params![stream_kind, stream_key, from, to],
                    StreamSegmentRow::from_row,
                )?;

                rows.collect()
            })
            .await
    }

    /// Every sealed segment whose last record is older than `before`, for the
    /// retention sweep to unlink.
    ///
    /// Only sealed segments: a file still being appended to is by definition
    /// not finished with, whatever its oldest record says.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn expired_before(
        &self,
        before: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<StreamSegmentRow>, Error> {
        let before = Timestamp::from(before);
        let limit = page_limit(limit);

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM stream_segments \
                     WHERE sealed = 1 AND last_time < ?1 ORDER BY last_time ASC LIMIT ?2"
                ))?;

                statement
                    .query_map(rusqlite::params![before, limit], StreamSegmentRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Every sealed segment a stream keeps only because nothing has evicted it,
    /// once the newer segments alone already hold `max_rows` records.
    ///
    /// The cap is per stream key — per device, for CoT — so that one talkative
    /// source cannot evict everybody else's history. The arithmetic is done
    /// here rather than in Rust because it is a window function over an index,
    /// and reading every segment row of every stream into memory to add up a
    /// column is the kind of thing that is fine until an installation has been
    /// running for a year.
    ///
    /// A segment is returned when the records in the segments *newer* than it
    /// already reach the cap: it is therefore entirely surplus, and the
    /// effective floor is `max_rows` plus the tail of the segment that
    /// straddles it — the same approximation the age horizon makes, and for the
    /// same reason. Only sealed segments: the one the writer is still appending
    /// to is the present, whatever the count says.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn over_row_cap(
        &self,
        stream_kind: &str,
        max_rows: u64,
        limit: usize,
    ) -> Result<Vec<StreamSegmentRow>, Error> {
        let stream_kind = stream_kind.to_owned();
        let limit = page_limit(limit);
        let cap = i64::try_from(max_rows).unwrap_or(i64::MAX);

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM (                        SELECT s.*, SUM(s.record_count) OVER (                          PARTITION BY s.stream_key                          ORDER BY s.last_time DESC, s.id DESC                          ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING                        ) AS newer                        FROM stream_segments s WHERE s.stream_kind = ?1                      ) WHERE sealed = 1 AND COALESCE(newer, 0) >= ?2                      ORDER BY stream_key ASC, last_time ASC LIMIT ?3"
                ))?;

                statement
                    .query_map(rusqlite::params![stream_kind, cap, limit], StreamSegmentRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Every sealed segment that is surplus once the newer segments of
    /// `stream_kind`, across **every** stream key, already hold `max_bytes`.
    ///
    /// The per-key row cap says nothing about a feed of many short-lived
    /// streams — an aircraft is a uid that reports for twenty minutes and never
    /// again — so this is the bound on the disk as a whole. Oldest first,
    /// whoever wrote it, and approximate in the keeping direction exactly as
    /// [`over_row_cap`](Self::over_row_cap) is. Open segments count towards the
    /// total and are never returned.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn over_byte_cap(
        &self,
        stream_kind: &str,
        max_bytes: u64,
        limit: usize,
    ) -> Result<Vec<StreamSegmentRow>, Error> {
        let stream_kind = stream_kind.to_owned();
        let limit = page_limit(limit);
        let cap = i64::try_from(max_bytes).unwrap_or(i64::MAX);

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM ( \
                       SELECT s.*, SUM(s.byte_length) OVER ( \
                         ORDER BY s.last_time DESC, s.id DESC \
                         ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING \
                       ) AS newer \
                       FROM stream_segments s WHERE s.stream_kind = ?1 \
                     ) WHERE sealed = 1 AND COALESCE(newer, 0) >= ?2 \
                     ORDER BY last_time ASC LIMIT ?3"
                ))?;

                statement
                    .query_map(
                        rusqlite::params![stream_kind, cap, limit],
                        StreamSegmentRow::from_row,
                    )?
                    .collect()
            })
            .await
    }

    /// Seals every segment of `stream_kind` nothing has appended to since
    /// `before`, reporting how many were closed.
    ///
    /// The writer parks a log rather than sealing it when it evicts one, so
    /// that the next message for that device continues the same file instead of
    /// starting a new one. The cost of that is a row left open, and retention
    /// only prunes sealed segments — so a device that never comes back would
    /// keep its last segment for ever. This is what closes them.
    ///
    /// Sealing a segment a live writer still holds is safe and rare: its next
    /// flush finds the row closed and rolls to a fresh segment
    /// ([`AppendLog::flush`](crate::store::AppendLog::flush)).
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn seal_idle(
        &self,
        stream_kind: &str,
        before: DateTime<Utc>,
    ) -> Result<usize, Error> {
        let stream_kind = stream_kind.to_owned();
        let before = Timestamp::from(before);

        let sealed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE stream_segments SET sealed = 1 \
                     WHERE stream_kind = ?1 AND sealed = 0 AND last_time < ?2",
                    rusqlite::params![stream_kind, before],
                )
            })
            .await?;

        Ok(sealed)
    }

    /// Forgets a segment, once its file has been unlinked.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, id: i64) -> Result<bool, Error> {
        let deleted = self
            .db
            .write(move |tx| tx.execute("DELETE FROM stream_segments WHERE id = ?1", [id]))
            .await?;

        Ok(deleted > 0)
    }

    /// Forgets a batch of segments in one transaction, reporting how many went.
    ///
    /// A sweep over a busy feed retires thousands of segments at once, and a
    /// commit each would queue every other writer behind it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete_many(&self, ids: Vec<i64>) -> Result<usize, Error> {
        if ids.is_empty() {
            return Ok(0);
        }

        self.db
            .write(move |tx| {
                let mut statement =
                    tx.prepare_cached("DELETE FROM stream_segments WHERE id = ?1")?;
                let mut deleted = 0;

                for id in &ids {
                    deleted += statement.execute([id])?;
                }

                Ok(deleted)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn opened(key: &str, path: &str, at: DateTime<Utc>) -> NewStreamSegment {
        NewStreamSegment {
            stream_kind: "cot".into(),
            stream_key: key.into(),
            segment_path: path.into(),
            first_time: at,
        }
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-18T10:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::TimeDelta::minutes(minutes)
    }

    #[tokio::test]
    async fn a_segment_starts_empty_and_open() {
        let db = db().await;

        let created = db
            .stream_segments()
            .create(opened("ANDROID-1", "cot/ANDROID-1/0001.log", at(0)))
            .await
            .unwrap();

        assert_eq!(created.record_count, 0);
        assert_eq!(created.byte_length, 0);
        assert!(!created.sealed);
        assert_eq!(created.first_time, created.last_time);
    }

    #[tokio::test]
    async fn a_path_is_indexed_once() {
        let db = db().await;
        db.stream_segments()
            .create(opened("ANDROID-1", "cot/a/0001.log", at(0)))
            .await
            .unwrap();

        assert!(
            db.stream_segments()
                .create(opened("ANDROID-2", "cot/a/0001.log", at(0)))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn appends_accumulate_and_move_the_last_time_forward_only() {
        let db = db().await;
        let segment = db
            .stream_segments()
            .create(opened("ANDROID-1", "cot/a/0001.log", at(0)))
            .await
            .unwrap();

        db.stream_segments()
            .record_append(segment.id, at(5), 10, 2_048)
            .await
            .unwrap();
        db.stream_segments()
            .record_append(segment.id, at(3), 5, 1_024)
            .await
            .unwrap();

        let read = db
            .stream_segments()
            .open_segment("cot", "ANDROID-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.record_count, 15);
        assert_eq!(read.byte_length, 3_072);
        assert_eq!(
            read.last_time,
            at(5),
            "an out-of-order frame must not rewind it"
        );
    }

    #[tokio::test]
    async fn a_sealed_segment_takes_no_more_appends_and_is_not_the_open_one() {
        let db = db().await;
        let segment = db
            .stream_segments()
            .create(opened("ANDROID-1", "cot/a/0001.log", at(0)))
            .await
            .unwrap();

        assert!(db.stream_segments().seal(segment.id).await.unwrap());
        assert!(!db.stream_segments().seal(segment.id).await.unwrap());

        assert!(
            !db.stream_segments()
                .record_append(segment.id, at(9), 1, 10)
                .await
                .unwrap()
        );
        assert!(
            db.stream_segments()
                .open_segment("cot", "ANDROID-1")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_query_finds_the_segments_its_window_touches() {
        let db = db().await;
        for (index, (start, end)) in [(0, 10), (10, 20), (20, 30)].into_iter().enumerate() {
            let segment = db
                .stream_segments()
                .create(opened(
                    "ANDROID-1",
                    &format!("cot/a/{index:04}.log"),
                    at(start),
                ))
                .await
                .unwrap();
            db.stream_segments()
                .record_append(segment.id, at(end), 100, 4_096)
                .await
                .unwrap();
            db.stream_segments().seal(segment.id).await.unwrap();
        }

        let found = db
            .stream_segments()
            .overlapping("cot", "ANDROID-1", at(15), at(25))
            .await
            .unwrap();

        assert_eq!(
            found.len(),
            2,
            "the window spans the second and third files"
        );
        assert_eq!(found[0].segment_path, "cot/a/0001.log");
        assert_eq!(found[1].segment_path, "cot/a/0002.log");

        assert!(
            db.stream_segments()
                .overlapping("cot", "ANDROID-2", at(0), at(30))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn retention_sees_sealed_segments_only() {
        let db = db().await;
        let old = db
            .stream_segments()
            .create(opened("ANDROID-1", "cot/a/0000.log", at(0)))
            .await
            .unwrap();
        db.stream_segments().seal(old.id).await.unwrap();
        db.stream_segments()
            .create(opened("ANDROID-1", "cot/a/0001.log", at(1)))
            .await
            .unwrap();

        let expired = db
            .stream_segments()
            .expired_before(at(30), 100)
            .await
            .unwrap();

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, old.id);

        assert!(db.stream_segments().delete(old.id).await.unwrap());
        assert!(!db.stream_segments().delete(old.id).await.unwrap());
    }
}
