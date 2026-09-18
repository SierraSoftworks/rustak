//! The migration runner.
//!
//! Migrations are plain `.sql` files under `rustak-server/migrations/`, compiled
//! into the binary with `include_dir!` so a release is one file with no
//! companion directory to lose. They are named `NNNN_snake_name.sql`, applied in
//! numeric order, each in its own transaction, and recorded in
//! `schema_migrations` as they go.
//!
//! # The rule
//!
//! **An applied migration is never edited.** Changing `0003` after a release has
//! run it leaves every existing installation on the old shape with no way to
//! tell, because the runner only looks at the highest id it has already applied.
//! A change is a new file. SQLite cannot alter a primary key in place, so a
//! table that has to be reshaped is rebuilt: create `<table>_migrated`, copy,
//! drop, rename — the pattern automate's `kv` rebuild uses.

use include_dir::{Dir, include_dir};
use rusqlite::Connection as SyncConnection;
use rustak_core::prelude::*;
use tokio_rusqlite::Connection;

use super::{ADVICE_DB_ERROR, ADVICE_REPORT_DEV, row::Timestamp};

static MIGRATIONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

/// How many digits the numeric prefix of a migration filename carries.
const PREFIX_DIGITS: usize = 4;

/// One migration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// Its numeric prefix, which is also its `schema_migrations.id`.
    pub id: usize,
    /// The filename, recorded so an error names the file rather than a number.
    pub name: &'static str,
    /// The statements it runs.
    pub sql: &'static str,
}

/// Every migration compiled into this binary, in the order they apply.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a file is misnamed, is not UTF-8,
/// or if the numbering has a gap or a duplicate — all of which are our mistakes
/// rather than the operator's, and all of which are caught by a unit test long
/// before a release.
pub fn load() -> Result<Vec<Migration>, Error> {
    let mut found: Vec<Migration> = Vec::new();

    for file in MIGRATIONS.files() {
        let name = file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| misnamed("a migration file has a name we cannot read"))?;

        found.push(Migration {
            id: parse_prefix(name)?,
            name,
            sql: file
                .contents_utf8()
                .ok_or_else(|| misnamed(format!("the migration '{name}' is not valid UTF-8")))?,
        });
    }

    found.sort_by_key(|migration| migration.id);

    for (index, migration) in found.iter().enumerate() {
        if migration.id != index + 1 {
            return Err(misnamed(format!(
                "the migrations are numbered with a gap or a duplicate: expected {} before '{}'",
                index + 1,
                migration.name
            )));
        }
    }

    Ok(found)
}

/// The numeric prefix of `NNNN_snake_name.sql`.
fn parse_prefix(name: &str) -> Result<usize, Error> {
    let rest = name
        .strip_suffix(".sql")
        .ok_or_else(|| misnamed(format!("the migration '{name}' does not end in '.sql'")))?;

    let (prefix, tail) = rest.split_at_checked(PREFIX_DIGITS).unwrap_or(("", rest));
    let well_formed = prefix.len() == PREFIX_DIGITS
        && prefix.bytes().all(|byte| byte.is_ascii_digit())
        && tail.starts_with('_')
        && tail.len() > 1
        && tail[1..]
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');

    if !well_formed {
        return Err(misnamed(format!(
            "the migration '{name}' is not named NNNN_snake_name.sql"
        )));
    }

    prefix
        .parse()
        .map_err(|_| misnamed(format!("the migration '{name}' has an unreadable number")))
}

fn misnamed(message: impl Into<String>) -> Error {
    human_errors::system(message.into(), ADVICE_REPORT_DEV)
}

/// Applies every migration the database has not yet had.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error naming the file that failed. The
/// failed file's transaction is rolled back, so the database is left at the last
/// migration that did apply rather than halfway through one.
#[instrument("db.migrate", skip_all, err(Display))]
pub async fn migrate(connection: &Connection) -> Result<(), Error> {
    migrate_to(connection, usize::MAX).await
}

/// Applies migrations up to and including `version`.
///
/// `usize::MAX` means "all of them". A lower number exists for tests that want
/// the schema as an older release left it, so that a migration can be run
/// against realistic data rather than against an empty table.
///
/// # Errors
///
/// As [`migrate`].
pub async fn migrate_to(connection: &Connection, version: usize) -> Result<(), Error> {
    let migrations = load()?;

    connection
        .call(|c| {
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                   id         INTEGER PRIMARY KEY,
                   name       TEXT NOT NULL,
                   applied_at TEXT NOT NULL
                 ) STRICT",
            )
        })
        .await
        .wrap_system_err(
            "Failed to prepare the database's migration table.",
            ADVICE_DB_ERROR,
        )?;

    let applied = current_version(connection).await?;

    for migration in migrations
        .iter()
        .filter(|migration| migration.id > applied && migration.id <= version)
    {
        let Migration { id, name, sql } = *migration;

        connection
            .call(move |c| {
                let transaction = c.transaction()?;
                transaction.execute_batch(sql)?;
                transaction.execute(
                    "INSERT INTO schema_migrations (id, name, applied_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id as i64, name, Timestamp::now()],
                )?;

                transaction.commit()
            })
            .await
            .wrap_system_err(
                format!("Failed to apply the database migration '{name}'."),
                ADVICE_REPORT_DEV,
            )?;

        info!(migration = name, "Applied a database migration.");
    }

    Ok(())
}

/// The highest migration this database has had applied, or 0 for a new one.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the migration table cannot be read.
pub async fn current_version(connection: &Connection) -> Result<usize, Error> {
    let version: i64 = connection
        .call(|c| {
            c.query_one(
                "SELECT COALESCE(MAX(id), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
        })
        .await
        .wrap_system_err(
            "Failed to read the database's migration version.",
            ADVICE_DB_ERROR,
        )?;

    Ok(version.max(0) as usize)
}

/// One table's shape, in a form two databases can be compared by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    /// `name type notnull default pk` for each column, in declaration order.
    pub columns: Vec<String>,
    /// The `CREATE INDEX` text of each index on the table, by name.
    pub indexes: Vec<String>,
}

/// Reads the shape of one table.
///
/// # Errors
///
/// Whatever SQLite reports; an unknown table simply has no columns.
pub fn schema_of(connection: &SyncConnection, table: &str) -> rusqlite::Result<TableSchema> {
    let mut info = connection.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk \
                                       FROM pragma_table_info(?1) ORDER BY cid",
    )?;
    let columns = info
        .query_map([table], |row| {
            Ok(format!(
                "{} {} notnull={} default={:?} pk={}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut indexes = connection.prepare(
        "SELECT COALESCE(sql, name) FROM sqlite_master \
         WHERE type = 'index' AND tbl_name = ?1 ORDER BY name",
    )?;
    let indexes = indexes
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(TableSchema { columns, indexes })
}

/// Every table in the database, excluding SQLite's own.
///
/// # Errors
///
/// Whatever SQLite reports.
pub fn tables(connection: &SyncConnection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn every_migration_is_named_and_numbered_without_a_gap() {
        let migrations = load().expect("the compiled-in migrations should load");

        assert!(!migrations.is_empty());
        for (index, migration) in migrations.iter().enumerate() {
            assert_eq!(
                migration.id,
                index + 1,
                "{} is out of order",
                migration.name
            );
            assert!(
                migration.name.starts_with(&format!("{:04}_", migration.id)),
                "{} should carry its own number",
                migration.name
            );
        }
    }

    #[test]
    fn a_misnamed_migration_is_refused() {
        for name in [
            "1_first.sql",
            "0001-first.sql",
            "0001_First.sql",
            "0001_first.txt",
            "0001_.sql",
            "notamigration.sql",
        ] {
            assert!(parse_prefix(name).is_err(), "{name} should be refused");
        }

        assert_eq!(parse_prefix("0012_add_stream_segments.sql").unwrap(), 12);
    }

    /// Timestamps are bound from Rust because SQLite's `CURRENT_TIMESTAMP`
    /// resolves only to the second and writes a space where RFC 3339 wants a
    /// `T`. This refuses the shortcut at the point it would be taken.
    #[test]
    fn no_migration_defaults_a_column_to_a_sql_generated_time() {
        for migration in load().unwrap() {
            // Comments are stripped first, so that a file may say why the rule
            // exists without tripping over it.
            let statements: String = migration
                .sql
                .lines()
                .map(|line| line.split("--").next().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
                .to_uppercase();

            for forbidden in ["CURRENT_TIMESTAMP", "CURRENT_DATE", "CURRENT_TIME"] {
                assert!(
                    !statements.contains(forbidden),
                    "{} uses {forbidden}; timestamps are bound from Rust",
                    migration.name
                );
            }
        }
    }

    #[tokio::test]
    async fn a_fresh_database_is_at_the_latest_migration() {
        let db = Database::open_in_memory().await.unwrap();
        let expected = load().unwrap().len() as i64;

        let (version, count) = db
            .read(|c| {
                c.query_one(
                    "SELECT COALESCE(MAX(id), 0), COUNT(*) FROM schema_migrations",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
            })
            .await
            .unwrap();

        assert_eq!(version, expected);
        assert_eq!(count, expected);
    }

    #[tokio::test]
    async fn migrating_twice_changes_nothing() {
        let db = Database::open_in_memory().await.unwrap();
        db.upgrade().await.unwrap();

        let count: i64 = db
            .read(|c| {
                c.query_one("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap();

        assert_eq!(count, load().unwrap().len() as i64);
    }

    #[tokio::test]
    async fn an_upgraded_database_has_the_same_schema_as_a_fresh_one() {
        let fresh = Database::open_in_memory().await.unwrap();

        for stop in 1..=load().unwrap().len() {
            let upgraded = Database::open_in_memory_at_migration(stop).await.unwrap();
            upgraded.upgrade().await.unwrap();

            let fresh_tables = fresh.read(tables).await.unwrap();
            let upgraded_tables = upgraded.read(tables).await.unwrap();
            assert_eq!(fresh_tables, upgraded_tables, "stopped after {stop}");

            for table in fresh_tables {
                let name = table.clone();
                let a = fresh.read(move |c| schema_of(c, &name)).await.unwrap();
                let name = table.clone();
                let b = upgraded.read(move |c| schema_of(c, &name)).await.unwrap();

                assert_eq!(a, b, "table '{table}' differs when stopped after {stop}");
            }
        }
    }

    #[tokio::test]
    async fn stopping_partway_leaves_the_later_tables_absent() {
        let db = Database::open_in_memory_at_migration(1).await.unwrap();

        assert_eq!(db.read(tables).await.unwrap().len(), 4);

        db.upgrade().await.unwrap();
        assert!(db.read(tables).await.unwrap().len() > 4);
    }

    #[tokio::test]
    async fn the_migrated_database_passes_sqlites_own_checks() {
        let db = Database::open_in_memory().await.unwrap();

        let violations: Vec<String> = db
            .read(|c| {
                let mut statement = c.prepare("PRAGMA foreign_key_check")?;
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect()
            })
            .await
            .unwrap();
        assert!(violations.is_empty(), "{violations:?}");

        let integrity: String = db
            .read(|c| c.query_one("PRAGMA integrity_check", [], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[tokio::test]
    async fn every_table_is_strict() {
        let db = Database::open_in_memory().await.unwrap();

        let lax: Vec<String> = db
            .read(|c| {
                let mut statement = c.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' \
                     AND name NOT LIKE 'sqlite_%' AND sql NOT LIKE '%STRICT%'",
                )?;
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect()
            })
            .await
            .unwrap();

        assert!(lax.is_empty(), "these tables are not STRICT: {lax:?}");
    }

    #[tokio::test]
    async fn the_plans_deltas_are_in_the_schema() {
        let db = Database::open_in_memory().await.unwrap();
        let names = db.read(tables).await.unwrap();

        assert!(!names.contains(&"cot_history".to_string()));
        assert!(names.contains(&"stream_segments".to_string()));
        assert!(names.contains(&"passkeys".to_string()));

        let columns = db
            .read(|c| schema_of(c, "users"))
            .await
            .unwrap()
            .columns
            .join(" ");
        assert!(
            !columns.contains("password_hash"),
            "rustak has no local passwords"
        );
    }
}
