//! The context tests run against.
//!
//! Everything here is compiled only under `cfg(test)` or the `testing` feature,
//! so an installed `rustak` binary carries none of it.

#![cfg(any(test, feature = "testing"))]

use std::sync::Arc;

use rustak_core::{prelude::*, telemetry::Session};

use super::AppContext;
use crate::{config::Config, crypto::SecretStore, db::Database};

impl AppContext {
    /// A context backed by an in-memory database, a throwaway encryption key
    /// and a telemetry session that records into memory.
    ///
    /// `f` adjusts the configuration before the context is built, which is how
    /// a test says what it is testing: `new_mock(|config| config.auth.audience
    /// = "example".into())`. The database is already migrated, so the seed rows
    /// the migrations write (the `__ANON__` group, and so on) are present.
    ///
    /// Nothing here touches the filesystem: the content store and the signing
    /// keys are left uninstalled, and a test that needs one installs it.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error if the in-memory
    /// database cannot be created or migrated, or if the shared HTTP client
    /// cannot be built.
    pub async fn new_mock(f: impl Sized + FnOnce(&mut Config)) -> Result<Self, Error> {
        let db = Database::open_in_memory().await?;

        let mut config = Config::default();
        f(&mut config);

        // Built here rather than through `rustak_core::telemetry::testing_session`,
        // which is behind that crate's own `testing` feature: reaching it would
        // mean adding a feature-enabling dev-dependency to this crate's manifest
        // for the three lines it saves.
        let session =
            Arc::new(Session::new("rustak", "0.0.0-test").with_battery(tracing_batteries::Testing));

        Self::new(
            config,
            db,
            SecretStore::ephemeral(),
            session,
            Shutdown::new(),
        )
    }
}
