//! `cot_latest`: one row per uid, holding the last thing it said.
//!
//! This is what `GET /Marti/api/cot/xml/{uid}` answers from, what the oversize
//! protobuf substitution points a client at, and what a Marti `sync` search
//! reads. The stored XML is the **relayed** form — `<marti>` stripped, this
//! server's flow tag present — so that a client fetching a message it missed
//! gets exactly the bytes its peers were sent.
//!
//! # Why the sender's channels are stored with the row
//!
//! A message read back tomorrow still has to answer "was this reader allowed to
//! see it?", and memberships change. Re-deriving the answer from today's
//! memberships would either leak a message to somebody who has since joined a
//! channel or hide one from somebody who has since left it. The sender's bit
//! vector at send time is the only thing that makes the question answerable
//! later, so it is written with the row.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension as _, params};

use crate::db::Database;
use crate::db::row::{Timestamp, opt_id_col, ts};
use crate::prelude::*;

use super::CotRecord;

/// One row of `cot_latest`.
#[derive(Clone, Debug, PartialEq)]
pub struct LatestRow {
    /// The event uid.
    pub uid: String,
    /// Its CoT type.
    pub kind: String,
    /// The sender's callsign.
    pub callsign: Option<String>,
    /// The account it came from.
    pub user_id: Option<UserId>,
    /// The device it came from.
    pub device_id: Option<DeviceId>,
    /// The sender's channel rights when it was sent.
    pub group_bits: Vec<u8>,
    /// The event's own time.
    pub time: DateTime<Utc>,
    /// When it goes stale.
    pub stale: DateTime<Utc>,
    /// The XML the recipients were sent.
    pub xml: String,
    /// When this server handled it.
    pub received_at: DateTime<Utc>,
}

impl LatestRow {
    /// The reader's half of the schema, in column order.
    pub(super) const COLUMNS: &'static str = "uid, type, callsign, user_id, device_id, group_bits, \
                                   time, stale, xml, received_at";

    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            uid: row.get(0)?,
            kind: row.get(1)?,
            callsign: row.get(2)?,
            user_id: opt_id_col(row, 3)?,
            device_id: opt_id_col(row, 4)?,
            group_bits: row.get(5)?,
            time: ts(row, 6)?,
            stale: ts(row, 7)?,
            xml: row.get(8)?,
            received_at: ts(row, 9)?,
        })
    }

    /// Whether a reader holding `groups` was allowed to see this message.
    pub fn visible_to(&self, groups: &GroupSet) -> bool {
        match GroupSet::from_bytes(&self.group_bits) {
            Ok(sender) => can_reach(&sender, groups),
            // A row written by a different schema is one we cannot reason
            // about, so it is not shown. Failing closed is the only safe
            // direction for a visibility check.
            Err(err) => {
                warn!(uid = %self.uid, error = %err, "A stored message has an unreadable channel set.");
                false
            }
        }
    }
}

/// Writes a batch of messages, replacing whatever each uid said before.
///
/// One transaction for the whole batch: the writer task collects whatever
/// arrived in the last window and pays for one commit rather than one per
/// message.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn upsert_batch(db: &Database, records: Vec<CotRecord>) -> Result<usize, Error> {
    if records.is_empty() {
        return Ok(0);
    }

    db.write(move |transaction| {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO cot_latest
                 (uid, type, callsign, user_id, device_id, group_bits,
                  time, start, stale, lat, lon, hae, ce, le, xml, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(uid) DO UPDATE SET
                 type = excluded.type, callsign = excluded.callsign,
                 user_id = excluded.user_id, device_id = excluded.device_id,
                 group_bits = excluded.group_bits, time = excluded.time,
                 start = excluded.start, stale = excluded.stale,
                 lat = excluded.lat, lon = excluded.lon, hae = excluded.hae,
                 ce = excluded.ce, le = excluded.le, xml = excluded.xml,
                 received_at = excluded.received_at
             WHERE excluded.time >= cot_latest.time",
        )?;

        let mut written = 0;

        for record in &records {
            let (lat, lon, hae, ce, le) = record.point;
            // `cot_latest.xml` is a TEXT column, and this is where the XML is
            // produced for a record that never went to an XML client —
            // on the writer's own task rather than on the sender's (R-03 M7).
            // Borrowed, not copied, whenever the bytes are valid UTF-8, which
            // they are unless a client sent something this server re-encoded.
            let xml = String::from_utf8_lossy(record.xml());

            written += statement.execute(params![
                record.uid,
                record.kind,
                record.callsign,
                record.user_id.map(i64::from),
                record.device_id.map(i64::from),
                record.group_bits,
                Timestamp::from(record.time),
                Timestamp::from(record.start),
                Timestamp::from(record.stale),
                lat,
                lon,
                hae,
                ce,
                le,
                xml.as_ref(),
                Timestamp::from(record.received_at),
            ])?;
        }

        Ok(written)
    })
    .await
}

/// The XML a uid last sent, if we still hold it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn latest_xml(db: &Database, uid: &str) -> Result<Option<String>, Error> {
    let uid = uid.to_owned();

    db.read(move |connection| {
        connection
            .query_row(
                "SELECT xml FROM cot_latest WHERE uid = ?1",
                params![uid],
                |row| row.get(0),
            )
            .optional()
    })
    .await
}

/// One whole row, for a caller that has to check visibility.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn latest_event(db: &Database, uid: &str) -> Result<Option<LatestRow>, Error> {
    let uid = uid.to_owned();
    let sql = format!(
        "SELECT {} FROM cot_latest WHERE uid = ?1",
        LatestRow::COLUMNS
    );

    db.read(move |connection| {
        connection
            .query_row(&sql, params![uid], LatestRow::from_row)
            .optional()
    })
    .await
}

/// Everything heard from since an instant, newest first.
///
/// `types` narrows by CoT type prefix — `a-` for atoms, `b-t-f` for chat — and
/// an empty list means every type. This is the read a Marti `sync` search and
/// the map's initial load are built on.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn latest_events(
    db: &Database,
    since: DateTime<Utc>,
    prefixes: &[String],
    limit: u32,
) -> Result<Vec<LatestRow>, Error> {
    let prefixes: Vec<String> = prefixes.to_vec();
    let sql = format!(
        "SELECT {} FROM cot_latest WHERE received_at >= ?1 ORDER BY received_at DESC LIMIT ?2",
        LatestRow::COLUMNS
    );

    db.read(move |connection| {
        let mut statement = connection.prepare_cached(&sql)?;
        let rows = statement
            .query_map(params![Timestamp::from(since), limit], LatestRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Filtered here rather than in SQL: a `LIKE` per prefix would defeat
        // the `received_at` index, and the row count is already bounded.
        Ok(rows
            .into_iter()
            .filter(|row| {
                prefixes.is_empty()
                    || prefixes
                        .iter()
                        .any(|prefix| row.kind.starts_with(prefix.as_str()))
            })
            .collect())
    })
    .await
}

/// Every row that was not yet stale at `since`, newest relayed first.
///
/// What a map draws. Narrowed by `stale` rather than by `received_at`, because
/// the two disagree in both directions: a marker dropped a week ago with a
/// year to live belongs on the map, and a position report relayed a minute ago
/// that went stale ten seconds later does not.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn current(
    db: &Database,
    since: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<LatestRow>, Error> {
    let sql = format!(
        "SELECT {} FROM cot_latest WHERE stale >= ?1 ORDER BY received_at DESC LIMIT ?2",
        LatestRow::COLUMNS
    );

    db.read(move |connection| {
        connection
            .prepare_cached(&sql)?
            .query_map(params![Timestamp::from(since), limit], LatestRow::from_row)?
            .collect()
    })
    .await
}

/// Forgets the rows whose messages went stale before `before`.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying whatever SQLite reported.
pub async fn prune_stale(db: &Database, before: DateTime<Utc>) -> Result<usize, Error> {
    db.write(move |transaction| {
        transaction.execute(
            "DELETE FROM cot_latest WHERE stale < ?1",
            params![Timestamp::from(before)],
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustak_cot::Event;
    use rustak_cot::codec::EncodedEvent;
    use rustak_cot::detail::{Contact, contact::STREAMING_ENDPOINT};

    use super::*;

    fn principal(grants: &[(u32, Direction)]) -> Principal {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        Principal::new(
            UserId::from(1),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
        .with_groups(std::sync::Arc::new(groups))
    }

    fn record(uid: &str, at: DateTime<Utc>, grants: &[(u32, Direction)]) -> CotRecord {
        let encoded = EncodedEvent::new(
            Event::builder("a-f-G-U-C", uid)
                .how("m-g")
                .point(51.5, -0.12)
                .time(rustak_cot::CotTime::from_millis(at.timestamp_millis()))
                .typed(&Contact::new("ALPHA").with_endpoint(STREAMING_ENDPOINT))
                .build(),
        );

        // The account row the foreign key points at is the one the shared
        // `db()` helper creates, so the record names it rather than a row id
        // that happens to be free.
        let mut record = CotRecord::new(Arc::new(encoded), &principal(grants), None);
        record.user_id = None;
        record
    }

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn the_latest_row_is_the_latest_thing_the_uid_said() {
        let db = db().await;
        let first = Utc::now() - chrono::Duration::seconds(30);
        let second = Utc::now();

        upsert_batch(&db, vec![record("UID-A", first, &[(7, Direction::In)])])
            .await
            .unwrap();
        upsert_batch(&db, vec![record("UID-A", second, &[(7, Direction::In)])])
            .await
            .unwrap();

        let row = latest_event(&db, "UID-A").await.unwrap().expect("a row");

        assert_eq!(row.uid, "UID-A");
        assert_eq!(row.time.timestamp_millis(), second.timestamp_millis());
        assert!(latest_xml(&db, "UID-A").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_message_that_arrives_out_of_order_does_not_rewind_the_row() {
        // A relay or a reconnect can deliver an older report after a newer one;
        // letting it win would move a contact backwards on every map.
        let db = db().await;
        let newer = Utc::now();
        let older = newer - chrono::Duration::seconds(60);

        upsert_batch(&db, vec![record("UID-A", newer, &[])])
            .await
            .unwrap();
        let written = upsert_batch(&db, vec![record("UID-A", older, &[])])
            .await
            .unwrap();

        assert_eq!(written, 0, "the older message changes nothing");
        assert_eq!(
            latest_event(&db, "UID-A")
                .await
                .unwrap()
                .unwrap()
                .time
                .timestamp_millis(),
            newer.timestamp_millis(),
        );
    }

    #[tokio::test]
    async fn a_stored_message_answers_who_was_allowed_to_see_it() {
        let db = db().await;
        upsert_batch(
            &db,
            vec![record("UID-A", Utc::now(), &[(7, Direction::In)])],
        )
        .await
        .unwrap();

        let row = latest_event(&db, "UID-A").await.unwrap().unwrap();

        let mut reader = GroupSet::new();
        reader.set(7, Direction::Out);
        assert!(row.visible_to(&reader));

        let mut stranger = GroupSet::new();
        stranger.set(9, Direction::Out);
        assert!(!row.visible_to(&stranger));
    }

    #[tokio::test]
    async fn an_unreadable_channel_set_hides_the_message_rather_than_showing_it() {
        let row = LatestRow {
            uid: "UID-A".into(),
            kind: "a-f-G".into(),
            callsign: None,
            user_id: None,
            device_id: None,
            group_bits: vec![0, 1, 2],
            time: Utc::now(),
            stale: Utc::now(),
            xml: String::new(),
            received_at: Utc::now(),
        };

        assert!(!row.visible_to(&GroupSet::new()));
    }

    #[tokio::test]
    async fn a_search_narrows_by_type_prefix() {
        let db = db().await;
        let now = Utc::now();
        let mut chat = record("UID-C", now, &[]);
        chat.kind = "b-t-f".into();

        upsert_batch(&db, vec![record("UID-A", now, &[]), chat])
            .await
            .unwrap();

        let since = now - chrono::Duration::hours(1);
        let atoms = latest_events(&db, since, &["a-".to_string()], 100)
            .await
            .unwrap();
        let everything = latest_events(&db, since, &[], 100).await.unwrap();

        assert_eq!(atoms.len(), 1);
        assert_eq!(atoms[0].uid, "UID-A");
        assert_eq!(everything.len(), 2);
    }

    #[tokio::test]
    async fn stale_rows_are_pruned() {
        let db = db().await;
        let mut old = record("UID-OLD", Utc::now(), &[]);
        old.stale = Utc::now() - chrono::Duration::days(30);

        upsert_batch(&db, vec![old, record("UID-NEW", Utc::now(), &[])])
            .await
            .unwrap();

        let removed = prune_stale(&db, Utc::now() - chrono::Duration::days(7))
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(latest_event(&db, "UID-OLD").await.unwrap().is_none());
        assert!(latest_event(&db, "UID-NEW").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn an_empty_batch_is_not_a_transaction() {
        let db = db().await;

        assert_eq!(upsert_batch(&db, Vec::new()).await.unwrap(), 0);
    }
}
