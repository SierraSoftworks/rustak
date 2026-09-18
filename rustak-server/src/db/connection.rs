//! Opening the database, and the pragmas every connection carries.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use rusqlite::{Connection as SyncConnection, OpenFlags, Transaction, TransactionBehavior};
use rustak_core::prelude::*;
use tokio_rusqlite::Connection;

use super::{ADVICE_DB_ERROR, migrations};

/// How long a connection waits for a lock another connection holds before
/// reporting `SQLITE_BUSY`.
///
/// Set explicitly rather than left to `rusqlite`'s own five-second default,
/// which is an undocumented implementation detail of a dependency governing a
/// setting whose absence turns routine contention into user-visible errors.
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many read-only connections a file-backed database opens by default.
///
/// Two is enough under WAL, where readers never block and never block anyone;
/// each additional one costs a thread for no extra concurrency, because the
/// work they serialise behind is the writer's.
pub const DEFAULT_READER_CONNECTIONS: usize = 2;

/// Cap on the write-ahead log before SQLite truncates it back after a
/// checkpoint. 64 MiB, which is far more than this schema's write volume needs
/// and small enough not to surprise an operator looking at the data directory.
const JOURNAL_SIZE_LIMIT: i64 = 67_108_864;

/// Which kind of checkpoint to ask SQLite for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    /// Fold what it can into the database without waiting for readers. Cheap
    /// enough to run periodically.
    Passive,
    /// Fold everything in and truncate the log to nothing. Run at shutdown so
    /// the data directory is left tidy and a copy of the file is complete.
    Truncate,
}

impl Checkpoint {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Passive => "PRAGMA wal_checkpoint(PASSIVE)",
            Self::Truncate => "PRAGMA wal_checkpoint(TRUNCATE)",
        }
    }
}

/// One writer connection and a small read-only pool over the same file.
#[derive(Clone)]
pub struct Database {
    writer: Arc<Connection>,
    readers: Arc<Readers>,
}

/// The read-only pool, handed out round-robin.
struct Readers {
    connections: Vec<Connection>,
    next: AtomicUsize,
}

impl Readers {
    fn pick(&self) -> Option<&Connection> {
        if self.connections.is_empty() {
            return None;
        }

        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.connections.len();

        self.connections.get(index)
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("readers", &self.readers.connections.len())
            .finish()
    }
}

impl Database {
    /// Opens (creating if necessary) the database at `path` and migrates it.
    ///
    /// Takes plain parameters rather than the server's `StorageConfig`, which
    /// is a sibling brief's type: the caller passes the three values that
    /// configuration will eventually supply.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the file cannot be opened —
    /// that is a path or a permission the operator can fix — and a
    /// [`human_errors::Kind::System`] error for anything else.
    #[instrument("db.open", skip_all, fields(readers = reader_connections), err(Display))]
    pub async fn open(
        path: &Path,
        reader_connections: usize,
        busy_timeout: Duration,
    ) -> Result<Self, Error> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await.wrap_user_err(
                format!("Unable to create the directory '{}'.", parent.display()),
                &["Check that the path in [storage] database is one rustak may write to."],
            )?;
        }

        let writer = Connection::open(path).await.wrap_user_err(
            format!("Unable to open the SQLite database '{}'.", path.display()),
            &[
                "Check that the path in [storage] database is correct.",
                "Check that the file and its directory are readable and writable by rustak.",
            ],
        )?;

        configure_writer(&writer, busy_timeout, true).await?;
        migrations::migrate(&writer).await?;

        let readers = open_readers(path, reader_connections, busy_timeout).await?;

        Ok(Self {
            writer: Arc::new(writer),
            readers: Arc::new(Readers {
                connections: readers,
                next: AtomicUsize::new(0),
            }),
        })
    }

    /// Opens a private in-memory database and migrates it, for tests.
    ///
    /// There is no read pool: an in-memory database belongs to the one
    /// connection that created it, so reads run on the writer.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the database cannot be created
    /// or migrated.
    pub async fn open_in_memory() -> Result<Self, Error> {
        let writer = Connection::open_in_memory()
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        configure_writer(&writer, DEFAULT_BUSY_TIMEOUT, false).await?;
        migrations::migrate(&writer).await?;

        Ok(Self {
            writer: Arc::new(writer),
            readers: Arc::new(Readers {
                connections: Vec::new(),
                next: AtomicUsize::new(0),
            }),
        })
    }

    /// Opens an in-memory database frozen partway through the migration
    /// history, so a test can seed it and then run the rest against real rows.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the database cannot be created
    /// or the migrations up to `version` do not apply.
    #[cfg(any(test, feature = "testing"))]
    pub async fn open_in_memory_at_migration(version: usize) -> Result<Self, Error> {
        let writer = Connection::open_in_memory()
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        configure_writer(&writer, DEFAULT_BUSY_TIMEOUT, false).await?;
        migrations::migrate_to(&writer, version).await?;

        Ok(Self {
            writer: Arc::new(writer),
            readers: Arc::new(Readers {
                connections: Vec::new(),
                next: AtomicUsize::new(0),
            }),
        })
    }

    /// Applies any migrations this database has not yet had.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error naming the migration that failed.
    #[cfg(any(test, feature = "testing"))]
    pub async fn upgrade(&self) -> Result<(), Error> {
        migrations::migrate(&self.writer).await
    }

    /// Runs a read on one of the read-only connections, or on the writer when
    /// there is no pool.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error carrying whatever SQLite or the
    /// row mapping reported.
    pub async fn read<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&SyncConnection) -> rusqlite::Result<T> + Send + 'static,
    {
        let connection = self.readers.pick().unwrap_or(&self.writer);

        connection
            .call(move |c| f(c))
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }

    /// Runs a write inside `BEGIN IMMEDIATE … COMMIT` on the writer.
    ///
    /// `IMMEDIATE` rather than the deferred default so that the write lock is
    /// taken up front: a transaction that reads first and then discovers it
    /// cannot upgrade has to be retried by the caller, which is a failure mode
    /// worth not having.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error carrying whatever SQLite or the
    /// closure reported. The transaction is rolled back on any error.
    pub async fn write<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut Transaction<'_>) -> rusqlite::Result<T> + Send + 'static,
    {
        self.writer
            .call(move |c| {
                let mut transaction =
                    c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let value = f(&mut transaction)?;
                transaction.commit()?;

                Ok::<_, rusqlite::Error>(value)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }

    /// Folds the write-ahead log back into the database file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if SQLite refuses the checkpoint.
    #[instrument("db.checkpoint", skip(self), fields(mode = ?mode), err(Display))]
    pub async fn checkpoint(&self, mode: Checkpoint) -> Result<(), Error> {
        self.writer
            .call(move |c| c.execute_batch(mode.as_sql()))
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }

    /// Lets SQLite update its statistics, truncates the log, and closes.
    ///
    /// The statistics come **first**. `PRAGMA optimize` runs `ANALYZE`, which is
    /// a write — so a `TRUNCATE` checkpoint before it leaves the log empty for
    /// exactly as long as it takes the next statement to fill it up again, and
    /// the data directory a stopped server leaves behind still has a
    /// `-wal` file in it.
    ///
    /// # What "draining the readers" does and does not mean
    ///
    /// `TRUNCATE` is the one checkpoint mode that can be refused outright: it
    /// needs every other connection to be done with the log, and a reader in
    /// the middle of a query holds a read mark that the checkpointer will not
    /// step past. It reports that as `SQLITE_BUSY` on the pragma rather than as
    /// an error, so a truncation that did not happen looks exactly like one
    /// that did — which is why this is worth spelling out.
    ///
    /// Each pooled read runs as its own statement on its own connection and the
    /// read mark is released when that statement finishes, so an *idle* reader
    /// holds nothing. Dropping them here is therefore belt and braces rather
    /// than the mechanism: what actually guarantees the truncation is that
    /// [`run_all`](crate::runtime::run_all) calls this after every listener has
    /// stopped, so there is no query left to be in the middle of. The drop is
    /// also only effective for the last `Database` handle — the server holds
    /// one on its context while this runs — and the truncation happens anyway,
    /// which is the property `the_log_is_truncated_while_the_server_still_holds_a_handle`
    /// pins down.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the final checkpoint fails.
    /// Failing to close the connection itself is logged rather than returned:
    /// by then the data is already durable and there is nothing a caller could
    /// usefully do about it.
    #[instrument("db.close", skip(self), err(Display))]
    pub async fn close(self) -> Result<(), Error> {
        let Self { writer, readers } = self;

        writer
            .call(|c| c.execute_batch("PRAGMA optimize"))
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        // The readers go before the checkpoint, not after: `TRUNCATE` waits for
        // every other connection to be done with the log, and an idle reader
        // still holding its snapshot is one it would wait out.
        drop(readers);

        writer
            .call(move |c| c.execute_batch(Checkpoint::Truncate.as_sql()))
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        // Joining the worker thread needs sole ownership of the writer, and on
        // every path this is reached from the application context still holds a
        // handle — it is shared with every task that has not finished unwinding
        // yet. That is not a durability problem: `PRAGMA optimize` and the
        // TRUNCATE checkpoint above both ran on this connection, so the file on
        // disk is already complete and the log is already folded in. The thread
        // ends when the last handle is dropped, which is moments later.
        match Arc::try_unwrap(writer) {
            Ok(writer) => {
                if let Err(err) = writer.close().await {
                    warn!(error = %err, "The database connection did not close cleanly.");
                }
            }
            Err(shared) => debug!(
                handles = Arc::strong_count(&shared),
                "The database worker thread will end when the last handle is dropped."
            ),
        }

        Ok(())
    }
}

/// Applies the pragmas the writer needs, reporting when WAL did not stick.
async fn configure_writer(
    connection: &Connection,
    busy_timeout: Duration,
    on_disk: bool,
) -> Result<(), Error> {
    let journal_mode = connection
        .call(move |c| {
            // First, deliberately: switching a database into WAL needs a brief
            // exclusive lock, and waiting a moment for it beats failing start-up.
            c.busy_timeout(busy_timeout)?;
            c.pragma_update(None, "foreign_keys", "ON")?;
            c.pragma_update(None, "temp_store", "MEMORY")?;

            if !on_disk {
                // WAL needs a file for the log and the shared-memory index, and
                // an in-memory database is private to this connection anyway.
                return Ok::<_, rusqlite::Error>(None);
            }

            let journal_mode: String =
                c.query_one("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;

            if journal_mode.eq_ignore_ascii_case("wal") {
                // Durable against this process dying, at the cost of the last
                // few commits if the machine loses power. Paired with the
                // rollback journal we fall back to on network storage it would
                // instead risk a torn journal, so it is gated on WAL applying.
                c.pragma_update(None, "synchronous", "NORMAL")?;
                c.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
            }

            Ok(Some(journal_mode))
        })
        .await
        .or_system_err(ADVICE_DB_ERROR)?;

    if let Some(mode) = journal_mode
        && !mode.eq_ignore_ascii_case("wal")
    {
        warn!(
            journal_mode = %mode,
            "The database could not be switched into write-ahead logging, so readers will be locked out while each write commits. This usually means it is on a network filesystem (NFS or SMB), which SQLite cannot use WAL on; moving it onto local storage will fix it."
        );
    }

    Ok(())
}

/// Opens the read-only pool. Called after the migrations, so the file exists.
async fn open_readers(
    path: &Path,
    count: usize,
    busy_timeout: Duration,
) -> Result<Vec<Connection>, Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let path: PathBuf = path.to_path_buf();
    let mut readers = Vec::with_capacity(count);

    for _ in 0..count {
        let reader = Connection::open_with_flags(&path, flags)
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        reader
            .call(move |c| {
                c.busy_timeout(busy_timeout)?;
                c.pragma_update(None, "foreign_keys", "ON")?;
                c.pragma_update(None, "temp_store", "MEMORY")?;
                // Belt and braces over the read-only open flag: a statement
                // that tries to write reports so here rather than at the file.
                c.pragma_update(None, "query_only", "ON")
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        readers.push(reader);
    }

    Ok(readers)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_database() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(
            &dir.path().join("rustak.sqlite"),
            DEFAULT_READER_CONNECTIONS,
            DEFAULT_BUSY_TIMEOUT,
        )
        .await
        .unwrap();

        (dir, db)
    }

    #[tokio::test]
    async fn a_file_database_runs_in_write_ahead_logging() {
        let (dir, db) = temp_database().await;

        let mode: String = db
            .read(|c| c.query_one("PRAGMA journal_mode", [], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        // Asked of the writer: `synchronous` is a per-connection setting, and
        // it is only the connection that writes which it governs.
        let synchronous: i64 = db
            .write(|tx| tx.query_one("PRAGMA synchronous", [], |row| row.get(0)))
            .await
            .unwrap();
        // 1 is NORMAL.
        assert_eq!(synchronous, 1);

        assert!(dir.path().join("rustak.sqlite-wal").exists());
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced_on_every_connection() {
        let (_dir, db) = temp_database().await;

        for _ in 0..DEFAULT_READER_CONNECTIONS + 1 {
            let on: i64 = db
                .read(|c| c.query_one("PRAGMA foreign_keys", [], |row| row.get(0)))
                .await
                .unwrap();
            assert_eq!(on, 1);
        }

        let refused = db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO devices (uid, user_id, first_seen_at, last_seen_at) \
                     VALUES ('d', 9999, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                    [],
                )
            })
            .await;

        assert!(refused.is_err(), "a dangling user_id should be refused");
    }

    #[tokio::test]
    async fn readers_refuse_to_write() {
        let (_dir, db) = temp_database().await;

        let refused = db
            .read(|c| c.execute("DELETE FROM groups WHERE id = 1", []))
            .await;

        assert!(refused.is_err());
    }

    #[tokio::test]
    async fn a_failed_write_leaves_nothing_behind() {
        let db = Database::open_in_memory().await.unwrap();

        let failed = db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO settings (key, value, updated_at) \
                     VALUES ('a', '1', '2026-01-01T00:00:00.000Z')",
                    [],
                )?;

                Err::<(), _>(rusqlite::Error::QueryReturnedNoRows)
            })
            .await;
        assert!(failed.is_err());

        let count: i64 = db
            .read(|c| c.query_one("SELECT COUNT(*) FROM settings", [], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(count, 0, "the rolled-back insert should not have survived");
    }

    #[tokio::test]
    async fn closing_truncates_the_write_ahead_log() {
        let (dir, db) = temp_database().await;
        let wal = dir.path().join("rustak.sqlite-wal");

        db.write(|tx| {
            tx.execute(
                "INSERT INTO settings (key, value, updated_at) \
                 VALUES ('server.name', '\"rustak\"', '2026-01-01T00:00:00.000Z')",
                [],
            )
        })
        .await
        .unwrap();

        db.close().await.unwrap();

        let size = std::fs::metadata(&wal).map(|meta| meta.len()).unwrap_or(0);
        assert_eq!(size, 0, "the log should have been folded back in");
    }

    #[tokio::test]
    async fn the_log_is_truncated_while_the_server_still_holds_a_handle() {
        // How `runtime::run_all` actually calls this: `context.db().clone()`,
        // with the context's own handle still alive. The `drop(readers)` inside
        // `close` therefore drops an `Arc` that is not the last one, so if the
        // truncation depended on the pool being *closed* rather than merely
        // idle, this is the test that would fail and the plain one above would
        // keep passing.
        let (dir, db) = temp_database().await;
        let wal = dir.path().join("rustak.sqlite-wal");

        db.write(|tx| {
            tx.execute(
                "INSERT INTO settings (key, value, updated_at) \
                 VALUES ('server.name', '\"rustak\"', '2026-01-01T00:00:00.000Z')",
                [],
            )
        })
        .await
        .unwrap();

        // A read on every pooled connection, so that each of them has had a
        // snapshot open at some point before the checkpoint.
        for _ in 0..DEFAULT_READER_CONNECTIONS {
            let _: i64 = db
                .read(|c| c.query_one("SELECT COUNT(*) FROM settings", [], |row| row.get(0)))
                .await
                .unwrap();
        }

        let held = db.clone();
        db.close().await.unwrap();

        let size = std::fs::metadata(&wal).map(|meta| meta.len()).unwrap_or(0);
        assert_eq!(size, 0, "an idle read pool must not downgrade the TRUNCATE");

        drop(held);
    }

    #[tokio::test]
    async fn an_in_memory_database_reads_through_the_writer() {
        let db = Database::open_in_memory().await.unwrap();

        assert!(db.readers.pick().is_none());

        let anon: String = db
            .read(|c| c.query_one("SELECT name FROM groups WHERE id = 1", [], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(anon, "__ANON__");
    }
}
