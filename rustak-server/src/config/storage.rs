//! `[storage]` — where state lives and how SQLite is opened.
//!
//! Three separate stores, because they have three different access patterns:
//! SQLite holds state and metadata, the content store holds immutable blobs
//! addressed by their hash, and the stream directory holds append-only segment
//! files for the CoT history. Each path defaults to a name under
//! `[server] data_dir`, so moving the data directory moves all three; setting
//! one explicitly is how an installation puts the blobs on a different volume
//! from the database.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The database file name under `data_dir`.
const DATABASE_NAME: &str = "rustak.sqlite";

/// The content-addressed blob directory name under `data_dir`.
const CONTENT_NAME: &str = "content";

/// The append-only stream segment directory name under `data_dir`.
const STREAMS_NAME: &str = "streams";

/// Read-only connections opened alongside the single writer.
fn default_reader_connections() -> usize {
    2
}

/// How long a statement waits for the writer before giving up.
fn default_busy_timeout() -> chrono::Duration {
    chrono::Duration::seconds(5)
}

/// How often the WAL is checkpointed.
fn default_checkpoint_interval() -> chrono::Duration {
    chrono::Duration::minutes(5)
}

/// `[storage]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// The SQLite database file. Defaults to `<data_dir>/rustak.sqlite`.
    ///
    /// The encryption key for sealed secrets is kept beside it, so moving this
    /// moves both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<PathBuf>,

    /// The content-addressed blob directory. Defaults to `<data_dir>/content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_dir: Option<PathBuf>,

    /// The append-only stream segment root. Defaults to `<data_dir>/streams`.
    ///
    /// CoT history is written here as length-prefixed frames in segment files
    /// rather than as SQLite rows, so that a busy installation's write volume
    /// never reaches the database. Retention prunes whole segments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams_dir: Option<PathBuf>,

    /// Read-only WAL connections opened alongside the single writer.
    ///
    /// `0` makes reads share the writer, which serialises them; it is here for
    /// a memory-constrained deployment rather than as an optimisation.
    #[serde(default = "default_reader_connections")]
    pub reader_connections: usize,

    /// How long a statement waits for the write lock before returning
    /// `SQLITE_BUSY`.
    #[serde(
        default = "default_busy_timeout",
        with = "rustak_core::config::duration::humane"
    )]
    pub busy_timeout: chrono::Duration,

    /// How often the PASSIVE WAL checkpoint job runs.
    ///
    /// The WAL is also truncated on a clean shutdown; this is what keeps it
    /// bounded on an installation that stays up for months.
    #[serde(
        default = "default_checkpoint_interval",
        with = "rustak_core::config::duration::humane"
    )]
    pub checkpoint_interval: chrono::Duration,
}

impl Default for StorageConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            database: None,
            content_dir: None,
            streams_dir: None,
            reader_connections: default_reader_connections(),
            busy_timeout: default_busy_timeout(),
            checkpoint_interval: default_checkpoint_interval(),
        }
    }
}

impl StorageConfig {
    /// The database path, resolved against the data directory.
    pub fn database(&self, data_dir: &Path) -> PathBuf {
        self.resolve(self.database.as_deref(), data_dir, DATABASE_NAME)
    }

    /// The content store path, resolved against the data directory.
    pub fn content_dir(&self, data_dir: &Path) -> PathBuf {
        self.resolve(self.content_dir.as_deref(), data_dir, CONTENT_NAME)
    }

    /// The stream segment root, resolved against the data directory.
    pub fn streams_dir(&self, data_dir: &Path) -> PathBuf {
        self.resolve(self.streams_dir.as_deref(), data_dir, STREAMS_NAME)
    }

    /// A configured path, or `<data_dir>/<name>`.
    ///
    /// A configured *relative* path is taken relative to the data directory
    /// too: `content_dir = "blobs"` means "beside the database", which is what
    /// somebody writing that meant, whereas resolving it against the process's
    /// working directory would put it wherever systemd happened to start us.
    fn resolve(&self, configured: Option<&Path>, data_dir: &Path, name: &str) -> PathBuf {
        match configured {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => data_dir.join(path),
            None => data_dir.join(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: StorageConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, StorageConfig::default());
        assert_eq!(parsed.reader_connections, 2);
        assert_eq!(parsed.busy_timeout, chrono::Duration::seconds(5));
        assert_eq!(parsed.checkpoint_interval, chrono::Duration::minutes(5));
    }

    #[test]
    fn the_three_stores_default_to_names_under_the_data_directory() {
        let storage = StorageConfig::default();
        let data_dir = Path::new("/var/lib/rustak");

        assert_eq!(
            storage.database(data_dir),
            PathBuf::from("/var/lib/rustak/rustak.sqlite")
        );
        assert_eq!(
            storage.content_dir(data_dir),
            PathBuf::from("/var/lib/rustak/content")
        );
        assert_eq!(
            storage.streams_dir(data_dir),
            PathBuf::from("/var/lib/rustak/streams")
        );
    }

    #[test]
    fn an_absolute_path_puts_a_store_on_another_volume() {
        let storage: StorageConfig = toml::from_str(r#"content_dir = "/mnt/blobs""#).unwrap();

        assert_eq!(
            storage.content_dir(Path::new("/var/lib/rustak")),
            PathBuf::from("/mnt/blobs")
        );
    }

    #[test]
    fn a_relative_path_is_taken_against_the_data_directory_not_the_cwd() {
        // Otherwise where the blobs land depends on how the process was
        // started, which is a difference nobody wants to debug.
        let storage: StorageConfig = toml::from_str(r#"content_dir = "blobs""#).unwrap();

        assert_eq!(
            storage.content_dir(Path::new("/var/lib/rustak")),
            PathBuf::from("/var/lib/rustak/blobs")
        );
    }

    #[test]
    fn durations_are_written_the_way_a_person_writes_them() {
        let storage: StorageConfig = toml::from_str(
            r#"
            busy_timeout = "30s"
            checkpoint_interval = "1h"
            "#,
        )
        .unwrap();

        assert_eq!(storage.busy_timeout, chrono::Duration::seconds(30));
        assert_eq!(storage.checkpoint_interval, chrono::Duration::hours(1));
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<StorageConfig>(r#"streams_directory = "x""#) else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("streams_directory"), "{err}");
    }
}
