//! The one task that writes what the stream relayed.
//!
//! It owns the database writer connection's turn and every open history
//! segment, which is what makes the batching possible: whatever arrived while
//! the last batch was being committed is committed together, so a burst of
//! fifty position reports costs one transaction rather than fifty.
//!
//! # Why it drains on a deadline as well as a count
//!
//! Waiting for a full batch would mean a quiet installation's single message
//! sat unwritten until the next one arrived, which could be minutes. Waiting
//! only on a timer would mean a busy one paid a wake-up per window whatever the
//! volume. Draining on whichever comes first — [`CotStoreOptions::batch`]
//! records or [`CotStoreOptions::window`] elapsed — costs one commit per window
//! at most and never leaves a message waiting longer than one.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::db::Database;
use crate::prelude::*;

use super::history::HistoryWriter;
use super::{CotRecord, CotStoreHandle, latest};

/// How the writer batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CotStoreOptions {
    /// How many messages may wait to be written before the router starts
    /// dropping them.
    pub queue_len: usize,
    /// The most records committed in one transaction.
    pub batch: usize,
    /// The longest a record waits for company.
    pub window: Duration,
    /// Whether history segments are written as well as `cot_latest`.
    pub history: bool,
}

impl Default for CotStoreOptions {
    fn default() -> Self {
        Self {
            queue_len: 4096,
            batch: 256,
            window: Duration::from_millis(50),
            history: true,
        }
    }
}

/// Starts the writer task, and hands back the end the router records through.
///
/// The task stops when `shutdown` is cancelled **or** when every handle has
/// been dropped, and flushes what it holds either way — a segment left with an
/// index that undercounts it is recoverable, but only at the cost of a scan on
/// the next start.
pub fn start(
    db: Database,
    streams_dir: impl Into<std::path::PathBuf>,
    options: CotStoreOptions,
    shutdown: Shutdown,
) -> (CotStoreHandle, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(options.queue_len.max(1));
    let history = HistoryWriter::new(db.clone(), streams_dir);
    let task = tokio::spawn(run(db, rx, history, options, shutdown));

    (CotStoreHandle::new(tx), task)
}

/// The writer loop.
async fn run(
    db: Database,
    mut rx: mpsc::Receiver<CotRecord>,
    mut history: HistoryWriter,
    options: CotStoreOptions,
    shutdown: Shutdown,
) {
    let mut batch: Vec<CotRecord> = Vec::with_capacity(options.batch);

    loop {
        let first = tokio::select! {
            biased;

            () = shutdown.cancelled() => None,
            record = rx.recv() => record,
        };

        let Some(first) = first else {
            break;
        };

        batch.push(first);
        fill(&mut rx, &mut batch, options).await;

        write(&db, &mut history, &mut batch, options).await;
    }

    // Whatever was queued when the signal arrived is still worth writing: it
    // has already been relayed, so it is history that happened.
    while let Ok(record) = rx.try_recv() {
        batch.push(record);

        if batch.len() >= options.batch {
            write(&db, &mut history, &mut batch, options).await;
        }
    }

    write(&db, &mut history, &mut batch, options).await;

    if let Err(err) = history.close().await {
        warn!(error = %err, "Could not close the CoT history segments cleanly.");
    }

    debug!("The CoT store has stopped.");
}

/// Takes whatever else is already waiting, up to the batch size or the window.
async fn fill(
    rx: &mut mpsc::Receiver<CotRecord>,
    batch: &mut Vec<CotRecord>,
    options: CotStoreOptions,
) {
    let deadline = tokio::time::Instant::now() + options.window;

    while batch.len() < options.batch {
        let next = tokio::time::timeout_at(deadline, rx.recv()).await;

        match next {
            Ok(Some(record)) => batch.push(record),
            // The channel closed, or the window expired: either way this is the
            // batch.
            Ok(None) | Err(_) => break,
        }
    }
}

/// Commits one batch, and empties it whatever happened.
///
/// A failure is logged rather than returned: the messages have already been
/// relayed, and retrying them forever would turn a database problem into a
/// growing queue and then into dropped *relays*.
async fn write(
    db: &Database,
    history: &mut HistoryWriter,
    batch: &mut Vec<CotRecord>,
    options: CotStoreOptions,
) {
    if batch.is_empty() {
        return;
    }

    if options.history {
        for record in batch.iter().filter(|record| record.is_historic()) {
            if let Err(err) = history
                .append(&record.uid, record.received_at, &record.proto)
                .await
            {
                warn!(uid = %record.uid, error = %err, "Could not append a message to the CoT history.");
            }
        }

        if let Err(err) = history.flush().await {
            warn!(error = %err, "Could not update the CoT history index.");
        }
    }

    let records = std::mem::take(batch);
    let count = records.len();

    match latest::upsert_batch(db, records).await {
        Ok(written) => trace!(
            written,
            seen = count,
            "Recorded a batch of relayed messages."
        ),
        Err(err) => warn!(error = %err, count, "Could not record a batch of relayed messages."),
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::Event;
    use rustak_cot::codec::EncodedEvent;
    use rustak_cot::detail::{Contact, contact::STREAMING_ENDPOINT};

    use super::super::latest::latest_event;
    use super::*;

    fn principal() -> Principal {
        Principal::new(
            UserId::from(1),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
    }

    fn record(uid: &str) -> CotRecord {
        let encoded = EncodedEvent::new(
            Event::builder("a-f-G-U-C", uid)
                .how("m-g")
                .point(51.5, -0.12)
                .typed(&Contact::new("ALPHA").with_endpoint(STREAMING_ENDPOINT))
                .build(),
        );

        // No account row: an in-memory database has none, and the foreign
        // key is what would otherwise refuse the insert.
        let mut record = CotRecord::new(&encoded, &principal(), None);
        record.user_id = None;
        record
    }

    /// Waits for the store to have written a uid, or gives up.
    async fn eventually(db: &Database, uid: &str) -> bool {
        for _ in 0..200 {
            if latest_event(db, uid).await.unwrap().is_some() {
                return true;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        false
    }

    #[tokio::test]
    async fn a_recorded_message_reaches_both_stores() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let shutdown = Shutdown::new();
        let (handle, task) = start(
            db.clone(),
            dir.path(),
            CotStoreOptions::default(),
            shutdown.clone(),
        );

        handle.record(record("UID-A"));

        assert!(eventually(&db, "UID-A").await, "the row should be written");

        shutdown.cancel();
        task.await.unwrap();

        let segments = db
            .stream_segments()
            .overlapping(
                super::super::STREAM_KIND,
                "UID-A",
                chrono::Utc::now() - chrono::Duration::hours(1),
                chrono::Utc::now() + chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert_eq!(segments.len(), 1, "one history segment for the one uid");
    }

    #[tokio::test]
    async fn what_was_queued_when_the_server_stopped_is_still_written() {
        // The messages were already relayed, so they are history that happened;
        // dropping them would make the record disagree with what the clients
        // saw.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let shutdown = Shutdown::new();
        let (handle, task) = start(
            db.clone(),
            dir.path(),
            CotStoreOptions {
                window: Duration::from_millis(500),
                ..CotStoreOptions::default()
            },
            shutdown.clone(),
        );

        handle.record(record("UID-A"));
        handle.record(record("UID-B"));
        shutdown.cancel();

        task.await.unwrap();

        assert!(latest_event(&db, "UID-A").await.unwrap().is_some());
        assert!(latest_event(&db, "UID-B").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn control_traffic_never_reaches_the_history_segments() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let shutdown = Shutdown::new();
        let (handle, task) = start(
            db.clone(),
            dir.path(),
            CotStoreOptions::default(),
            shutdown.clone(),
        );

        // `msgs::ping` builds its uid as `{device}-ping`, which is the uid the
        // row is keyed on.
        let ping = EncodedEvent::new(rustak_cot::msgs::ping("UID-P", rustak_cot::CotTime::now()));
        let uid = ping.event().uid.clone();
        let mut control = CotRecord::new(&ping, &principal(), None);
        control.user_id = None;
        handle.record(control);
        assert!(eventually(&db, &uid).await);

        shutdown.cancel();
        task.await.unwrap();

        let segments = db
            .stream_segments()
            .overlapping(
                super::super::STREAM_KIND,
                &uid,
                chrono::Utc::now() - chrono::Duration::hours(1),
                chrono::Utc::now() + chrono::Duration::hours(1),
            )
            .await
            .unwrap();

        assert!(segments.is_empty(), "a ping is not history");
    }

    #[tokio::test]
    async fn turning_history_off_still_keeps_the_latest_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let shutdown = Shutdown::new();
        let (handle, task) = start(
            db.clone(),
            dir.path(),
            CotStoreOptions {
                history: false,
                ..CotStoreOptions::default()
            },
            shutdown.clone(),
        );

        handle.record(record("UID-A"));
        assert!(eventually(&db, "UID-A").await);

        shutdown.cancel();
        task.await.unwrap();

        assert!(
            db.stream_segments()
                .overlapping(
                    super::super::STREAM_KIND,
                    "UID-A",
                    chrono::Utc::now() - chrono::Duration::hours(1),
                    chrono::Utc::now() + chrono::Duration::hours(1),
                )
                .await
                .unwrap()
                .is_empty()
        );
    }
}
