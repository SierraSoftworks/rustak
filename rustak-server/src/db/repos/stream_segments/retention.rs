//! What the retention sweep reads, a bounded page at a time.
//!
//! A child module rather than more lines in [`super`], which is near
//! `conventions.md`'s length limit, and because these three reads share a
//! constraint nothing else in the repo has: **what they cost in memory must not
//! depend on how large the index is.** The first sweep of an installation far
//! past a limit it has only just been given can find millions of surplus
//! segments, on the micro-servers this is written for.
//!
//! Two things keep that true, and the tests below hold both.
//!
//! - Every read takes a `limit` and returns at most that many rows; the caller
//!   ([`AppendLog::remove_paged`]) asks again until a page comes back short.
//! - Every read walks an index in the order it wants its rows, so SQLite never
//!   sorts or materialises the table to answer it. That is not a nicety: a
//!   server runs with `temp_store = MEMORY`, so a sort *is* an allocation. The
//!   caps used to be window functions over the whole index, which cost about
//!   200 MiB per million segments per page however few rows came back. Now a
//!   cap is one streaming aggregate (how far over is it?) and one ordered walk
//!   from the oldest segment, stopped in Rust once enough has been found.
//!
//! The indexes are named in the SQL (`INDEXED BY`) so that the plan is a
//! property of the query rather than of the statistics `PRAGMA optimize` last
//! gathered — and so that dropping one is an error at once, not a regression
//! somebody finds in a memory graph.
//!
//! [`AppendLog::remove_paged`]: crate::store::AppendLog::remove_paged

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use crate::db::row::Timestamp;

use super::{COLUMNS, StreamSegmentRow, StreamSegmentsRepo};

/// Sealed segments by age; migration 0023.
const AGE_INDEX: &str = "idx_stream_segments_sealed_age";

/// Segments by stream, then by when each began; migration 0004.
const STREAM_INDEX: &str = "idx_stream_segments_stream";

/// How many bytes a stream kind holds, open segments included.
const KIND_BYTES_SQL: &str =
    "SELECT COALESCE(SUM(byte_length), 0) FROM stream_segments WHERE stream_kind = ?1";

/// Sealed segments of any kind older than `?1`, oldest first.
fn expired_sql() -> String {
    format!(
        "SELECT {COLUMNS} FROM stream_segments INDEXED BY {AGE_INDEX} \
         WHERE sealed = 1 AND last_time < ?1 ORDER BY last_time ASC, id ASC LIMIT ?2"
    )
}

/// Sealed segments of one kind, oldest first.
fn oldest_sealed_sql() -> String {
    format!(
        "SELECT {COLUMNS} FROM stream_segments INDEXED BY {AGE_INDEX} \
         WHERE sealed = 1 AND stream_kind = ?1 ORDER BY last_time ASC, id ASC LIMIT ?2"
    )
}

/// The stream keys of one kind holding more than `?2` records, with how many.
fn over_cap_streams_sql() -> String {
    format!(
        "SELECT stream_key, SUM(record_count) FROM stream_segments INDEXED BY {STREAM_INDEX} \
         WHERE stream_kind = ?1 GROUP BY stream_key HAVING SUM(record_count) > ?2"
    )
}

/// One stream's sealed segments, oldest first.
///
/// By `first_time`, which is the order the index holds them in. A stream's
/// segments are written one after another, so that is their age order too.
fn stream_oldest_sql() -> String {
    format!(
        "SELECT {COLUMNS} FROM stream_segments INDEXED BY {STREAM_INDEX} \
         WHERE stream_kind = ?1 AND stream_key = ?2 AND sealed = 1 \
         ORDER BY first_time ASC, id ASC LIMIT ?3"
    )
}

/// A count as SQLite binds it.
fn bind(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Moves rows from the front of `rows` into `surplus` for as long as their
/// running `weight` stays within `excess`.
///
/// `rows` is oldest first and `excess` is how far over its cap the whole is, so
/// what is taken is exactly what can go while the cap's worth still remains:
/// the row that would cross the line stays, which is the tail every retention
/// limit here documents keeping.
fn take_within(
    rows: impl Iterator<Item = rusqlite::Result<StreamSegmentRow>>,
    excess: u64,
    weight: impl Fn(&StreamSegmentRow) -> u64,
    surplus: &mut Vec<StreamSegmentRow>,
) -> rusqlite::Result<()> {
    let mut running = 0u64;

    for row in rows {
        let row = row?;
        running = running.saturating_add(weight(&row));

        if running > excess {
            break;
        }

        surplus.push(row);
    }

    Ok(())
}

impl StreamSegmentsRepo<'_> {
    /// The oldest `limit` sealed segments whose last record predates `before`.
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

        self.db
            .read(move |c| {
                c.prepare(&expired_sql())?
                    .query_map(
                        rusqlite::params![before, bind(limit)],
                        StreamSegmentRow::from_row,
                    )?
                    .collect()
            })
            .await
    }

    /// Up to `limit` sealed segments their streams could lose and still hold
    /// `max_rows` records each, oldest first within each stream.
    ///
    /// The cap is per stream key — per device, for CoT — so that one talkative
    /// source cannot evict everybody else's history. A stream's open segment
    /// counts towards what it holds and is never returned: it is the present.
    /// Whole segments go, so a stream keeps the cap plus the tail of the
    /// segment that straddles it; the same approximation the age horizon makes.
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
        let cap = i64::try_from(max_rows).unwrap_or(i64::MAX);

        self.db
            .read(move |c| {
                let mut streams = c.prepare(&over_cap_streams_sql())?;
                let mut oldest = c.prepare(&stream_oldest_sql())?;
                let mut surplus = Vec::new();

                // Streamed, never collected: there may be as many streams over
                // the cap as there are streams.
                let mut over = streams.query(rusqlite::params![stream_kind, cap])?;

                while surplus.len() < limit
                    && let Some(stream) = over.next()?
                {
                    let key: String = stream.get(0)?;
                    let held: i64 = stream.get(1)?;
                    let excess = u64::try_from(held.saturating_sub(cap)).unwrap_or(0);
                    let room = bind(limit - surplus.len());

                    let rows = oldest.query_map(
                        rusqlite::params![stream_kind, key, room],
                        StreamSegmentRow::from_row,
                    )?;
                    take_within(rows, excess, |row| row.record_count, &mut surplus)?;
                }

                Ok(surplus)
            })
            .await
    }

    /// Up to `limit` sealed segments of `stream_kind`, oldest first across
    /// **every** stream key, that could go and still leave `max_bytes` behind.
    ///
    /// The per-key row cap says nothing about a feed of many short-lived
    /// streams — an aircraft is a uid that reports for twenty minutes and never
    /// again — so this is the bound on the disk as a whole. Open segments count
    /// towards the total and are never returned, and it is approximate in the
    /// keeping direction exactly as [`over_row_cap`](Self::over_row_cap) is.
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

        self.db
            .read(move |c| {
                let held: i64 = c.query_row(KIND_BYTES_SQL, [&stream_kind], |row| row.get(0))?;
                let excess = u64::try_from(held).unwrap_or(0).saturating_sub(max_bytes);
                let mut surplus = Vec::new();

                if excess > 0 {
                    let mut oldest = c.prepare(&oldest_sealed_sql())?;
                    let rows = oldest.query_map(
                        rusqlite::params![stream_kind, bind(limit)],
                        StreamSegmentRow::from_row,
                    )?;
                    take_within(rows, excess, |row| row.byte_length, &mut surplus)?;
                }

                Ok(surplus)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, repos::NewStreamSegment};

    /// A sealed segment of `key` holding `records` records, `age` minutes old.
    async fn sealed(db: &Database, key: &str, index: u32, records: u64, age: i64) {
        let at = Utc::now() - chrono::TimeDelta::minutes(age);
        let segments = db.stream_segments();
        let row = segments
            .create(NewStreamSegment {
                stream_kind: "cot".into(),
                stream_key: key.into(),
                segment_path: format!("cot/{key}/{index:08}.log"),
                first_time: at,
            })
            .await
            .unwrap();

        segments
            .record_append(row.id, at, records, records * 100)
            .await
            .unwrap();
        segments.seal(row.id).await.unwrap();
    }

    #[tokio::test]
    async fn no_retention_read_sorts_or_materialises_the_index() {
        // The property the module exists for. A sort is an allocation the size
        // of what is sorted (`temp_store = MEMORY`), and the index can be
        // millions of rows; an ordered walk of an index is not.
        let db = Database::open_in_memory().await.unwrap();
        let reads: [(&str, String, usize); 5] = [
            ("expired", expired_sql(), 2),
            ("oldest sealed", oldest_sealed_sql(), 2),
            ("bytes held", KIND_BYTES_SQL.to_owned(), 1),
            ("streams over the cap", over_cap_streams_sql(), 2),
            ("one stream's oldest", stream_oldest_sql(), 3),
        ];

        for (name, sql, parameters) in reads {
            let plan: Vec<String> = db
                .read(move |c| {
                    let bound = vec![rusqlite::types::Null; parameters];
                    c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
                        .query_map(rusqlite::params_from_iter(bound), |row| row.get(3))?
                        .collect()
                })
                .await
                .unwrap();

            for step in &plan {
                for costly in ["TEMP B-TREE", "MATERIALIZE", "CO-ROUTINE"] {
                    assert!(!step.contains(costly), "{name}: {plan:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn a_page_is_never_longer_than_its_limit_however_much_is_surplus() {
        // Two streams with three surplus segments each; the limit falls in the
        // middle of the second.
        let db = Database::open_in_memory().await.unwrap();
        for key in ["UID-A", "UID-B"] {
            for index in 0..4 {
                sealed(&db, key, index, 10, 60 - i64::from(index)).await;
            }
        }
        let segments = db.stream_segments();

        let by_rows = segments.over_row_cap("cot", 10, 4).await.unwrap();
        let by_bytes = segments.over_byte_cap("cot", 1_000, 4).await.unwrap();
        let by_age = segments.expired_before(Utc::now(), 4).await.unwrap();

        assert_eq!(by_rows.len(), 4);
        assert_eq!(by_bytes.len(), 4);
        assert_eq!(by_age.len(), 4);
    }

    #[tokio::test]
    async fn a_cap_keeps_at_least_what_it_promises_and_takes_the_oldest() {
        let db = Database::open_in_memory().await.unwrap();
        for index in 0..4 {
            sealed(&db, "UID-A", index, 10, 60 - i64::from(index)).await;
        }
        let segments = db.stream_segments();

        // Forty records held, twenty-five promised: one segment can go, and a
        // second would leave only twenty.
        let by_rows = segments.over_row_cap("cot", 25, 100).await.unwrap();
        // Four thousand bytes held, two thousand promised: exactly two can go.
        let by_bytes = segments.over_byte_cap("cot", 2_000, 100).await.unwrap();

        let paths = |rows: &[StreamSegmentRow]| -> Vec<String> {
            rows.iter().map(|row| row.segment_path.clone()).collect()
        };
        assert_eq!(paths(&by_rows), ["cot/UID-A/00000000.log"]);
        assert_eq!(
            paths(&by_bytes),
            ["cot/UID-A/00000000.log", "cot/UID-A/00000001.log"]
        );
    }
}
