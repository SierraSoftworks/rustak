//! The sweep that collects blobs nothing refers to any more.
//!
//! The content store is addressed by hash and written *before* the row that
//! refers to it, which is what makes an upload atomic: the bytes are either
//! entirely there or not there at all, and a failure part-way through leaves a
//! file rather than a half-written row. The cost of that order is that a
//! failure — or a delete, or a purge — leaves a blob behind, and nothing in the
//! database says so. This is what notices.
//!
//! # Why a grace period, and why it is not zero
//!
//! A blob is referenced a moment *after* it is stored. A sweep that ran in
//! between would delete the file out from under the request that had just
//! written it, so `[retention] content_orphans` holds anything younger than its
//! horizon back — 24 hours by default, which is far longer than any upload and
//! far shorter than anybody notices the space.
//!
//! # The order of the two reads is the safety
//!
//! The blobs are listed **first** and the references read **second**. A blob
//! stored after the listing is not a candidate at all; a blob referenced
//! between the two appears in the reference set. Both orderings that could lose
//! data are therefore impossible, and the one that remains — a reference
//! removed after the read — only means the blob waits for the next sweep.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use rustak_core::prelude::*;

use super::content::ContentStore;
use crate::db::Database;

/// Every hash the database still refers to, from every table that holds one.
///
/// `mission_contents` is not here: it refers to a `resources` row rather than
/// to a hash, so it is already covered. `acme_certificates` holds its PEM
/// inline and never touches the content store.
const REFERENCED_HASHES: &str = "SELECT lower(hash) FROM resources \
     UNION SELECT lower(hash) FROM profile_files \
     UNION SELECT lower(content_hash) FROM mission_changes WHERE content_hash IS NOT NULL \
     UNION SELECT lower(value) FROM mission_logs, json_each(mission_logs.content_hashes)";

/// What one sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Blobs no row referred to.
    pub blobs: usize,
    /// How many bytes those blobs held.
    pub bytes: u64,
    /// Partly written uploads left in `tmp/` by a kill or a crash.
    pub temporary: usize,
}

impl Swept {
    /// Whether the sweep found anything at all.
    pub fn is_empty(self) -> bool {
        self.blobs == 0 && self.temporary == 0
    }
}

/// Removes every blob older than `grace` that nothing refers to, and every
/// temporary file older than `grace`.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the references cannot be read,
/// and a [`human_errors::Kind::User`] error when the store cannot be listed. A
/// single file that cannot be unlinked is logged and left, so the next sweep
/// tries again.
#[instrument("store.orphans.sweep", skip_all, err(Display))]
pub async fn sweep(
    db: &Database,
    store: &ContentStore,
    grace: chrono::Duration,
) -> Result<Swept, Error> {
    let cutoff = cutoff(grace);
    let mut swept = Swept {
        temporary: sweep_temporary(store, cutoff).await?,
        ..Swept::default()
    };

    let blobs = store.iter().await?;

    if blobs.is_empty() {
        return Ok(swept);
    }

    // Second, deliberately: see the module documentation.
    let referenced = referenced_hashes(db).await?;

    for blob in blobs {
        if referenced.contains(&blob.hash) {
            continue;
        }

        let path = store.path_for(&blob.hash)?;

        if !older_than(&path, cutoff).await {
            continue;
        }

        match store.remove(&blob.hash).await {
            Ok(true) => {
                swept.blobs += 1;
                swept.bytes += blob.size;
            }
            Ok(false) => {}
            Err(err) => {
                warn!(error = %err, "Could not remove an unreferenced stored file; it will be retried.");
            }
        }
    }

    Ok(swept)
}

/// Removes partly written uploads nothing is still writing to.
///
/// `put` cleans up after itself on every path it can reach, so what is left
/// here was left by a `SIGKILL`, an out-of-memory kill or a container eviction
/// mid-upload — which for a 400 MB data package is 400 MB held for the life of
/// the volume with nothing to say why.
async fn sweep_temporary(store: &ContentStore, cutoff: SystemTime) -> Result<usize, Error> {
    let temp = store.temp_dir();

    let mut entries = match tokio::fs::read_dir(&temp).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(err).wrap_user_err(
                format!(
                    "We could not list the temporary upload directory '{}'.",
                    temp.display()
                ),
                &["Check that the directory in [storage] content_dir is readable."],
            );
        }
    };

    let mut removed = 0;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();

        if !matches!(entry.file_type().await, Ok(kind) if kind.is_file()) {
            continue;
        }

        if !older_than(&path, cutoff).await {
            continue;
        }

        match tokio::fs::remove_file(&path).await {
            Ok(()) => removed += 1,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(error = %err, "Could not remove an abandoned upload; it will be retried.");
            }
        }
    }

    Ok(removed)
}

/// Every hash the database still refers to.
async fn referenced_hashes(db: &Database) -> Result<HashSet<String>, Error> {
    db.read(move |connection| {
        let mut statement = connection.prepare(REFERENCED_HASHES)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;

        rows.collect()
    })
    .await
}

/// The instant a file has to predate to be swept.
fn cutoff(grace: chrono::Duration) -> SystemTime {
    let grace = grace.to_std().unwrap_or(Duration::ZERO);

    SystemTime::now()
        .checked_sub(grace)
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Whether a file was last written before `cutoff`.
///
/// A file whose modification time cannot be read is treated as *not* old
/// enough: the sweep deletes things, so every uncertainty resolves towards
/// keeping them.
async fn older_than(path: &std::path::Path, cutoff: SystemTime) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };

    metadata.modified().is_ok_and(|at| at < cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Backdates a file so a sweep with a real grace period can see it.
    fn backdate(path: &std::path::Path, by: Duration) {
        let at = SystemTime::now() - by;
        let times = std::fs::FileTimes::new().set_modified(at);

        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }

    async fn store() -> (tempfile::TempDir, ContentStore, Database) {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::new(dir.path());

        store.prepare().await.unwrap();

        (dir, store, Database::open_in_memory().await.unwrap())
    }

    /// The one row shape a blob needs to count as referenced.
    async fn reference(db: &Database, hash: &str) {
        let hash = hash.to_owned();

        db.write(move |tx| {
            tx.execute(
                "INSERT INTO resources (hash, uid, name, mime_type, size, submission_time, created_at) \
                 VALUES (?1, ?1, 'package.zip', 'application/zip', 4, '2026-09-18T10:00:00.000Z', \
                         '2026-09-18T10:00:00.000Z')",
                [hash],
            )
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_blob_a_row_still_refers_to_is_kept() {
        let (dir, store, db) = store().await;
        let stored = store.put_bytes(b"keep").await.unwrap();

        reference(&db, &stored.hash).await;
        backdate(
            &store.path_for(&stored.hash).unwrap(),
            Duration::from_secs(60),
        );

        let swept = sweep(&db, &store, chrono::Duration::seconds(1))
            .await
            .unwrap();

        assert!(swept.is_empty());
        assert!(store.exists(&stored.hash).await.unwrap());

        drop(dir);
    }

    #[tokio::test]
    async fn a_blob_nothing_refers_to_is_removed() {
        // The leak H5 describes: the last referencing row is gone — a deleted
        // package, a purged mission — and the bytes stay for ever.
        let (dir, store, db) = store().await;
        let stored = store.put_bytes(b"gone").await.unwrap();

        backdate(
            &store.path_for(&stored.hash).unwrap(),
            Duration::from_secs(60),
        );

        let swept = sweep(&db, &store, chrono::Duration::seconds(1))
            .await
            .unwrap();

        assert_eq!(swept.blobs, 1);
        assert_eq!(swept.bytes, 4);
        assert!(!store.exists(&stored.hash).await.unwrap());

        drop(dir);
    }

    #[tokio::test]
    async fn a_blob_younger_than_the_grace_period_is_kept() {
        // An upload is referenced a moment after it is stored, so a sweep that
        // ran in between would delete the file out from under the request that
        // had just written it.
        let (dir, store, db) = store().await;
        let stored = store.put_bytes(b"new!").await.unwrap();

        let swept = sweep(&db, &store, chrono::Duration::hours(24))
            .await
            .unwrap();

        assert!(swept.is_empty());
        assert!(store.exists(&stored.hash).await.unwrap());

        drop(dir);
    }

    #[tokio::test]
    async fn an_upload_a_kill_abandoned_is_removed() {
        // `put` cleans up after itself; a SIGKILL or an eviction mid-upload
        // does not, and a 400 MB package then sits in tmp/ for the life of the
        // volume with nothing in the database to say why the disk is full.
        let (dir, store, db) = store().await;
        let abandoned = store
            .temp_dir()
            .join("11111111-2222-3333-4444-555555555555");

        tokio::fs::write(&abandoned, b"half an upload")
            .await
            .unwrap();
        backdate(&abandoned, Duration::from_secs(60));

        let swept = sweep(&db, &store, chrono::Duration::seconds(1))
            .await
            .unwrap();

        assert_eq!(swept.temporary, 1);
        assert!(!tokio::fs::try_exists(&abandoned).await.unwrap());

        drop(dir);
    }

    #[tokio::test]
    async fn every_table_that_holds_a_hash_is_asked() {
        // The query joins four tables, one of them through `json_each`. A typo
        // or a missing JSON extension would only show up here.
        let (dir, store, db) = store().await;

        assert!(referenced_hashes(&db).await.unwrap().is_empty());
        assert!(
            sweep(&db, &store, chrono::Duration::hours(1))
                .await
                .unwrap()
                .is_empty()
        );

        drop(dir);
    }
}
