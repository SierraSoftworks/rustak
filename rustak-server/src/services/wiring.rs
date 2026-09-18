//! Building the database and the secret store from configuration.
//!
//! Both `db` and `crypto` deliberately take plain parameters rather than the
//! configuration types: they are the layers underneath this one, and a
//! `Database` that knows what `[storage]` looks like is a `Database` that
//! cannot be opened by a test, a migration tool or a repair command without
//! constructing a whole `Config` first. The translation belongs here, with the
//! rest of the start-up wiring, and is two functions long.
//!
//! These are inherent constructors on the types they build — Rust allows an
//! inherent `impl` anywhere in the crate that defines the type — so that the
//! call site in `runtime` reads `Database::from_config(…)` rather than naming a
//! helper module.

use std::path::Path;

use rustak_core::prelude::*;

use crate::{
    config::{AuthConfig, StorageConfig},
    crypto::SecretStore,
    db::{Database, connection::DEFAULT_BUSY_TIMEOUT},
};

impl Database {
    /// Opens and migrates the database `[storage]` describes.
    ///
    /// `data_dir` is `[server] data_dir`, which the relative paths in
    /// `[storage]` resolve against; passing it separately keeps this a function
    /// of the section it is named for rather than of the whole file.
    ///
    /// # Errors
    ///
    /// As [`Database::open`]: a [`Kind::User`](human_errors::Kind::User) error
    /// when the file cannot be opened, and a
    /// [`Kind::System`](human_errors::Kind::System) error for anything else.
    pub async fn from_config(storage: &StorageConfig, data_dir: &Path) -> Result<Self, Error> {
        // A negative timeout is the only way this conversion fails, and
        // `Config::validate` refuses one; falling back rather than failing keeps
        // a validation gap from being an unstartable server.
        let busy_timeout = storage
            .busy_timeout
            .to_std()
            .unwrap_or(DEFAULT_BUSY_TIMEOUT);

        Self::open(
            &storage.database(data_dir),
            storage.reader_connections,
            busy_timeout,
        )
        .await
    }
}

impl SecretStore {
    /// Loads the keys `[auth]` describes, generating one beside the database
    /// when none is configured.
    ///
    /// `database_path` is where the generated key file is put — beside the
    /// database rather than inside it, so that a copy of the database alone is
    /// not a copy of everything it protects.
    ///
    /// # Errors
    ///
    /// As [`SecretStore::load`]: a [`Kind::User`](human_errors::Kind::User)
    /// error naming the key that could not be read.
    pub fn from_config(auth: &AuthConfig, database_path: &Path) -> Result<Self, Error> {
        Self::load(
            auth.secret_key.as_deref(),
            &auth.previous_secret_keys,
            database_path,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{config::Config, db::KeyValueStore};

    #[tokio::test]
    async fn the_database_is_opened_where_the_configuration_says() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::testing(dir.path());

        let db = Database::from_config(&config.storage, &config.server.data_dir)
            .await
            .unwrap();
        db.set("test", "opened", true).await.unwrap();
        db.close().await.unwrap();

        assert!(
            dir.path().join("rustak.sqlite").is_file(),
            "the default database name should have been created under the data directory",
        );
    }

    #[tokio::test]
    async fn a_relative_storage_path_is_taken_against_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::testing(dir.path());
        config.storage.database = Some("state/rustak.sqlite".into());

        Database::from_config(&config.storage, &config.server.data_dir)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();

        assert!(dir.path().join("state/rustak.sqlite").is_file());
    }

    #[test]
    fn the_secret_store_generates_a_key_beside_the_database_when_none_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::testing(dir.path());
        let database = config.database_path();

        let store = SecretStore::from_config(&config.auth, &database).unwrap();

        assert!(
            crate::crypto::key_file_for(&database).is_file(),
            "a generated key should have been written beside the database",
        );

        // The second load finds the key it wrote rather than making another.
        let reloaded = SecretStore::from_config(&config.auth, &database).unwrap();
        assert_eq!(store.active_key_id(), reloaded.active_key_id());
    }

    #[test]
    fn a_retired_key_that_cannot_be_read_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::testing(dir.path());
        config.auth.previous_secret_keys = vec!["not a key".to_string()];

        let refused = SecretStore::from_config(&config.auth, &config.database_path());

        assert!(refused.is_err());
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("previous_secret_keys")
        );
    }
}
