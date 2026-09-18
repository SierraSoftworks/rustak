//! The reads behind the administrator's CoT browser, and the one delete.
//!
//! [`latest`](latest()) pages `cot_latest`, [`history`] walks the append-only
//! segments a uid wrote, and [`forget`] removes both. Nothing here is on the
//! routing path: these are operator-facing reads that may take a lock and do a
//! little work, which is exactly what [`super::latest`] and [`super::history`]
//! must never do.
//!
//! # Why the history read does not open an [`AppendLog`](crate::store::AppendLog)
//!
//! [`AppendLog::open`](crate::store::AppendLog::open) *recovers*: it scans the
//! newest segment, truncates it back to the last complete record and reconciles
//! the index against the file. That is right for the writer, which owns the
//! stream, and wrong for a reader — the writer is holding the same segment open
//! and a reader that truncated it would destroy a record that had already been
//! acknowledged. So a read resolves the segments through the index and walks
//! the files, and never touches either.
//!
//! The open segment is asked for by name as well as by window, because the
//! index lags the file by up to one flush: a uid that reported a second ago has
//! its bytes on disk and its `last_time` still a batch behind.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::params;
use rustak_cot::Event;

use crate::db::Database;
use crate::db::row::Timestamp;
use crate::prelude::*;
use crate::store::frame::Frames;

use super::STREAM_KIND;
use super::latest::LatestRow;

/// The most rows one page of the browser may carry.
pub const MAX_PAGE: u32 = 200;

/// The most history events one read will ever return.
///
/// A hard ceiling rather than a page size: the caller clamps to its own page,
/// and this is what stops a caller that does not from asking for a device's
/// entire two-million-frame history in one allocation.
pub const MAX_HISTORY_ROWS: usize = 10_000;

/// The most stored records one history read will decode.
///
/// The other bound on [`history`]: a window whose in-range records are sparse —
/// a device that reported once an hour inside a week of somebody else's
/// traffic — would otherwise walk every segment it touches looking for a page
/// it will never fill.
const MAX_SCANNED_RECORDS: usize = 200_000;

/// Which of the stored messages a listing wants.
///
/// The textual predicates are decided in SQL so that a page is a page. The
/// channel rule is **not** — a sender's channels are a bit vector SQLite cannot
/// test — so a caller who is not an administrator applies
/// [`LatestRow::visible_to`] to what comes back and may see a short page, in
/// the same way `files::search` does for the same reason.
#[derive(Clone, Debug, Default)]
pub struct LatestQuery {
    /// A CoT type prefix: `a-` for atoms, `b-t-f` for chat.
    pub kind: Option<String>,
    /// Part of a callsign, matched without regard to case.
    pub callsign: Option<String>,
    /// The earliest relay time to include, when the caller named a window.
    ///
    /// Decided in SQL rather than over the page, unlike the channel rule: a
    /// window that ended an hour ago has none of its rows in the newest page,
    /// so narrowing afterwards would answer "nothing happened" for every
    /// question about the past.
    pub since: Option<DateTime<Utc>>,
    /// The latest relay time to include.
    pub until: Option<DateTime<Utc>>,
    /// How many rows, already clamped by the caller.
    pub limit: u32,
    /// How many rows to skip.
    pub offset: u32,
}

/// A page of `cot_latest`, most recently relayed first.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn latest(db: &Database, query: LatestQuery) -> Result<Vec<LatestRow>, Error> {
    let limit = query.limit.clamp(1, MAX_PAGE);
    let sql = format!(
        "SELECT {} FROM cot_latest \
         WHERE (?1 IS NULL OR type LIKE ?1 || '%') \
           AND (?2 IS NULL OR callsign LIKE '%' || ?2 || '%') \
           AND (?3 IS NULL OR received_at >= ?3) \
           AND (?4 IS NULL OR received_at <= ?4) \
         ORDER BY received_at DESC, uid ASC LIMIT ?5 OFFSET ?6",
        LatestRow::COLUMNS
    );
    let since = query.since.map(Timestamp::from);
    let until = query.until.map(Timestamp::from);

    db.read(move |connection| {
        let mut statement = connection.prepare_cached(&sql)?;

        statement
            .query_map(
                params![
                    query.kind,
                    query.callsign,
                    since,
                    until,
                    limit,
                    query.offset
                ],
                LatestRow::from_row,
            )?
            .collect()
    })
    .await
}

/// Every message one uid sent inside a window, newest first.
///
/// Segment-granular on the way in and precise on the way out: the index knows
/// which files touch the window, and only a decoded payload knows its own time.
/// A record that cannot be decoded is counted and dropped rather than failing
/// the request — one corrupt frame in a segment must not hide the rest of a
/// device's history.
///
/// # Why the walk is newest first, and stops
///
/// `limit` used to be applied *last*: every record of every segment the window
/// touched was read off disk and decoded into a full [`Event`] with its detail
/// tree before two hundred of them were kept. The per-device floor is
/// `[retention] cot_history_max_rows` — two million frames — so an
/// administrator asking for the last week of a busy device could allocate a
/// gigabyte to answer one page. Segments are time-ordered, so walking them
/// newest first means the newest `limit` events are found in the newest files
/// and everything older can be left on disk. A scan ceiling bounds the other
/// direction: a window whose in-range records are sparse still ends.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the index cannot be read, and a
/// [`human_errors::Kind::User`] error when a segment file cannot be read.
pub async fn history(
    db: &Database,
    streams_dir: &Path,
    uid: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<Event>, Error> {
    let limit = limit.clamp(1, MAX_HISTORY_ROWS);
    let mut events = Vec::new();
    let mut undecodable = 0usize;
    let mut scanned = 0usize;
    let mut truncated = false;

    for path in segment_paths(db, uid, from, to).await?.into_iter().rev() {
        let bytes = read_segment(streams_dir, &path).await?;

        // Decoded straight out of the borrowed frame: copying every record into
        // its own `Vec` first was a second full copy of the file, per file.
        for payload in Frames::new(&bytes) {
            scanned += 1;

            match decode(payload) {
                Some(event) if within(&event, from, to) => events.push(event),
                Some(_) => {}
                None => undecodable += 1,
            }
        }

        if events.len() >= limit {
            truncated = true;
            break;
        }

        if scanned >= MAX_SCANNED_RECORDS {
            truncated = true;
            warn!(
                uid = %uid,
                scanned,
                "Stopped reading CoT history at the scan limit; the window may hold more."
            );
            break;
        }
    }

    if undecodable > 0 {
        warn!(
            uid = %uid,
            records = undecodable,
            "Skipped stored CoT records that could not be decoded."
        );
    }

    if truncated {
        debug!(uid = %uid, scanned, "Answered a CoT history page without reading the whole window.");
    }

    events.sort_by_key(|event| std::cmp::Reverse(event.time));
    events.truncate(limit);

    Ok(events)
}

/// What a delete took with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Forgotten {
    /// Whether there was a `cot_latest` row.
    pub latest: bool,
    /// How many history segment files were removed.
    pub segments: usize,
}

/// Forgets everything this server holds about one uid.
///
/// The file goes before its index row, as retention does it: a row without its
/// file is recoverable and a file without its row is invisible and leaks. A
/// file the writer still holds open is unlinked anyway — on every platform
/// rustak runs on the writer keeps writing into an inode nothing can reach,
/// and its next segment is a fresh one.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the index or the row cannot be
/// written.
pub async fn forget(db: &Database, streams_dir: &Path, uid: &str) -> Result<Forgotten, Error> {
    let rows = db
        .stream_segments()
        .overlapping(STREAM_KIND, uid, epoch(), far_future())
        .await?;

    let mut segments = 0;

    for row in rows {
        let path = streams_dir.join(&row.segment_path);

        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(segment = %row.segment_path, error = %err, "Could not unlink a CoT history segment.");
                continue;
            }
        }

        if db.stream_segments().delete(row.id).await? {
            segments += 1;
        }
    }

    let key = uid.to_owned();
    let removed = db
        .write(move |transaction| {
            transaction.execute("DELETE FROM cot_latest WHERE uid = ?1", params![key])
        })
        .await?;

    Ok(Forgotten {
        latest: removed > 0,
        segments,
    })
}

/// The relative paths of the segments that could hold this window.
async fn segment_paths(
    db: &Database,
    uid: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<String>, Error> {
    let mut paths: Vec<String> = db
        .stream_segments()
        .overlapping(STREAM_KIND, uid, from, to)
        .await?
        .into_iter()
        .map(|row| row.segment_path)
        .collect();

    // The open segment's row lags the file by up to one flush, so it is asked
    // for by name rather than left to the window.
    if let Some(open) = db.stream_segments().open_segment(STREAM_KIND, uid).await?
        && !paths.contains(&open.segment_path)
    {
        paths.push(open.segment_path);
    }

    Ok(paths)
}

/// One segment file's bytes, for the caller to walk as frames.
///
/// Returned whole rather than as a `Vec` of owned records: [`Frames`] borrows,
/// and copying every record out of the buffer first was a second full copy of
/// an eight-megabyte file for a caller that decodes and drops most of them.
async fn read_segment(root: &Path, relative: &str) -> Result<Vec<u8>, Error> {
    let path = root.join(relative);

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            warn!(segment = %relative, "A CoT history segment is indexed but its file is gone.");
            return Ok(Vec::new());
        }
        Err(err) => {
            return Err(err).wrap_user_err(
                format!("We could not read the CoT history segment '{relative}'."),
                &["Check that the stream directory is readable by the server."],
            );
        }
    };

    Ok(bytes)
}

/// One stored payload as an event, or [`None`] for a record we cannot read.
fn decode(payload: &[u8]) -> Option<Event> {
    rustak_cot::proto::decode(payload)
        .ok()
        .and_then(|message| rustak_cot::proto::message_to_event(message).ok())
}

/// Whether an event's own time falls inside the window that was asked for.
fn within(event: &Event, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    event
        .time
        .to_datetime()
        .is_some_and(|at| at >= from && at <= to)
}

/// The start of the epoch, as a lower bound a segment cannot precede.
fn epoch() -> DateTime<Utc> {
    DateTime::from_timestamp(0, 0).unwrap_or_else(Utc::now)
}

/// Far enough ahead that no segment's first record is after it.
fn far_future() -> DateTime<Utc> {
    Utc::now() + chrono::Duration::days(365 * 100)
}

#[cfg(test)]
mod tests {
    use rustak_cot::CotTime;
    use rustak_cot::codec::EncodedEvent;
    use rustak_cot::detail::{Contact, Group, contact::STREAMING_ENDPOINT};

    use super::super::{CotRecord, latest::upsert_batch};
    use super::*;
    use crate::store::{AppendLog, AppendLogOptions};

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn principal(bits: &[u32]) -> Principal {
        let mut groups = GroupSet::new();
        for bitpos in bits {
            groups.set(*bitpos, Direction::In);
        }

        Principal::new(
            UserId::from(1),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
        .with_groups(std::sync::Arc::new(groups))
    }

    fn event(uid: &str, kind: &str, callsign: &str, at: DateTime<Utc>) -> EncodedEvent {
        EncodedEvent::new(
            rustak_cot::Event::builder(kind, uid)
                .how("m-g")
                .point(51.5, -0.12)
                .time(CotTime::from_datetime(at))
                .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
                .typed(&Group::new("Cyan", "Team Member"))
                .build(),
        )
    }

    fn record(uid: &str, kind: &str, callsign: &str, at: DateTime<Utc>, bits: &[u32]) -> CotRecord {
        let mut record = CotRecord::new(&event(uid, kind, callsign, at), &principal(bits), None);
        // The foreign key points at accounts this in-memory database has none
        // of, and none of these reads is about the sender's row.
        record.user_id = None;
        record.received_at = at;
        record
    }

    #[tokio::test]
    async fn a_listing_is_newest_first_and_narrows_by_type_and_callsign() {
        let db = db().await;
        let now = Utc::now();

        upsert_batch(
            &db,
            vec![
                record(
                    "UID-A",
                    "a-f-G-U-C",
                    "ALPHA",
                    now - chrono::Duration::seconds(60),
                    &[],
                ),
                record("UID-B", "a-f-G-U-C", "BRAVO", now, &[]),
                record("UID-C", "b-t-f", "ALPHA", now, &[]),
            ],
        )
        .await
        .unwrap();

        let all = latest(
            &db,
            LatestQuery {
                limit: 50,
                ..LatestQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(all.len(), 3);
        assert_ne!(all[2].uid, "UID-B", "the oldest row is last");

        let atoms = latest(
            &db,
            LatestQuery {
                kind: Some("a-".to_string()),
                limit: 50,
                ..LatestQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(atoms.len(), 2);

        let alpha = latest(
            &db,
            LatestQuery {
                callsign: Some("alph".to_string()),
                limit: 50,
                ..LatestQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(alpha.len(), 2, "a callsign match ignores case");
    }

    #[tokio::test]
    async fn a_page_is_a_window_and_its_size_is_capped() {
        let db = db().await;
        let now = Utc::now();

        for index in 0..5 {
            upsert_batch(
                &db,
                vec![record(
                    &format!("UID-{index}"),
                    "a-f-G",
                    "ALPHA",
                    now - chrono::Duration::seconds(index),
                    &[],
                )],
            )
            .await
            .unwrap();
        }

        let first = latest(
            &db,
            LatestQuery {
                limit: 2,
                ..LatestQuery::default()
            },
        )
        .await
        .unwrap();
        let second = latest(
            &db,
            LatestQuery {
                limit: 2,
                offset: 2,
                ..LatestQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        assert!(
            first
                .iter()
                .all(|row| !second.iter().any(|other| other.uid == row.uid)),
            "two pages of one listing do not overlap",
        );

        let asked_for_everything = latest(
            &db,
            LatestQuery {
                limit: u32::MAX,
                ..LatestQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            asked_for_everything.len(),
            5,
            "a huge limit is a page, not a refusal"
        );
    }

    #[tokio::test]
    async fn history_comes_back_newest_first_and_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let db = db().await;
        let now = Utc::now();

        let mut log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();

        for offset in [600i64, 120, 30] {
            let at = now - chrono::Duration::seconds(offset);
            log.append(at, event("UID-A", "a-f-G-U-C", "ALPHA", at).proto())
                .await
                .unwrap();
        }
        log.flush().await.unwrap();

        let window = history(
            &db,
            dir.path(),
            "UID-A",
            now - chrono::Duration::seconds(300),
            now,
            100,
        )
        .await
        .unwrap();

        assert_eq!(
            window.len(),
            2,
            "the ten-minute-old message is outside the window"
        );
        assert!(
            window[0].time >= window[1].time,
            "history is newest first, so a browser's first row is the last thing that happened",
        );
        assert_eq!(window[0].callsign(), Some("ALPHA"));

        let capped = history(
            &db,
            dir.path(),
            "UID-A",
            now - chrono::Duration::hours(1),
            now,
            1,
        )
        .await
        .unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].time, window[0].time, "a limit keeps the newest");
    }

    #[tokio::test]
    async fn a_page_of_history_stops_once_it_has_one() {
        // H4: `limit` used to be applied after every record of every segment in
        // the window had been read off disk and decoded. The older segments
        // here are replaced by directories, so a read that still walked the
        // whole window would fail rather than quietly cost a gigabyte.
        let dir = tempfile::tempdir().unwrap();
        let db = db().await;
        let now = Utc::now();

        let mut log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            // One record per segment, so "segments" and "records" are the same
            // number and the walk under test is visible.
            AppendLogOptions {
                max_segment_bytes: 1,
            },
        )
        .await
        .unwrap();

        for offset in (0..6i64).rev() {
            let at = now - chrono::Duration::seconds(offset * 10);
            log.append(at, event("UID-A", "a-f-G-U-C", "ALPHA", at).proto())
                .await
                .unwrap();
        }
        log.flush().await.unwrap();

        let mut rows = db
            .stream_segments()
            .overlapping(STREAM_KIND, "UID-A", epoch(), far_future())
            .await
            .unwrap();
        rows.sort_by_key(|row| row.first_time);
        assert_eq!(rows.len(), 6);

        for row in rows.iter().take(3) {
            let path = dir.path().join(&row.segment_path);
            tokio::fs::remove_file(&path).await.unwrap();
            tokio::fs::create_dir(&path).await.unwrap();
        }

        let page = history(
            &db,
            dir.path(),
            "UID-A",
            now - chrono::Duration::hours(1),
            now,
            2,
        )
        .await
        .expect("a page of two must not read the whole window");

        assert_eq!(page.len(), 2);
        assert!(page[0].time >= page[1].time);

        // And the whole window is still an error rather than a silent short
        // answer, so the stop above is the walk stopping and not a read that
        // swallows failures.
        assert!(
            history(
                &db,
                dir.path(),
                "UID-A",
                now - chrono::Duration::hours(1),
                now,
                100,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_decoded_does_not_hide_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let db = db().await;
        let now = Utc::now();

        let mut log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        log.append(now, b"not a protobuf message at all")
            .await
            .unwrap();
        log.append(now, event("UID-A", "a-f-G", "ALPHA", now).proto())
            .await
            .unwrap();
        log.flush().await.unwrap();

        let read = history(
            &db,
            dir.path(),
            "UID-A",
            now - chrono::Duration::minutes(5),
            now + chrono::Duration::minutes(5),
            100,
        )
        .await
        .unwrap();

        assert_eq!(read.len(), 1);
    }

    #[tokio::test]
    async fn forgetting_a_uid_takes_the_row_and_the_segments() {
        let dir = tempfile::tempdir().unwrap();
        let db = db().await;
        let now = Utc::now();

        upsert_batch(&db, vec![record("UID-A", "a-f-G", "ALPHA", now, &[])])
            .await
            .unwrap();

        let mut log = AppendLog::open(
            &db,
            dir.path(),
            STREAM_KIND,
            "UID-A",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        log.append(now, event("UID-A", "a-f-G", "ALPHA", now).proto())
            .await
            .unwrap();
        log.seal().await.unwrap();

        let forgotten = forget(&db, dir.path(), "UID-A").await.unwrap();

        assert!(forgotten.latest);
        assert_eq!(forgotten.segments, 1);
        assert!(
            super::super::latest::latest_event(&db, "UID-A")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            history(
                &db,
                dir.path(),
                "UID-A",
                now - chrono::Duration::hours(1),
                now,
                10
            )
            .await
            .unwrap()
            .is_empty()
        );

        assert_eq!(
            forget(&db, dir.path(), "UID-A").await.unwrap(),
            Forgotten::default(),
            "forgetting twice reports that there was nothing there",
        );
    }
}
