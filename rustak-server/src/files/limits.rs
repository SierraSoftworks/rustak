//! The upload ceiling, and where it comes from.
//!
//! One number, read by four places: `/files/api/config` (which is the gate
//! CloudTAK's setup wizard will not save a connection past), the two
//! `/Marti/sync` upload servlets, and the admin API's own package upload. It
//! therefore has exactly one resolver, so that what a client is *told* and what
//! it is actually *allowed* cannot disagree.
//!
//! # The configuration file wins, and says so
//!
//! `[marti] upload_size_limit_mb` is what an operator who deploys a file writes,
//! and a web form does not get to quietly replace it — the same rule
//! [`crate::identity::settings`] applies to the server's identity. So the file
//! wins wherever it says anything, [`from_config_file`](rustak_api::FileSettings::from_config_file) reports
//! that it did, and a `PUT` that would be overridden at the next start is
//! refused rather than stored and ignored.
//!
//! "Says anything" is "differs from the built-in default", which is the same
//! test the wizard's settings use: a file that repeats the default has not
//! expressed a preference, and an installation that has never been given one
//! should still be able to raise its limit from the admin UI.

use rustak_api::FileSettings;
use rustak_core::prelude::*;

use crate::config::{Config, MartiConfig};
use crate::db::Database;

/// The `settings` row the admin API writes.
pub const SETTINGS_KEY: &str = "files";

/// The largest limit an operator may set from the admin API, in megabytes.
///
/// Not a technical ceiling — uploads stream — but a number past which a
/// mistyped form field becomes a way to fill the disk before the bound in the
/// reader ever matters.
pub const MAX_LIMIT_MB: u32 = 4_096;

/// What this installation's file limits actually are.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the stored row cannot be read.
pub async fn resolve(config: &Config, db: &Database) -> Result<FileSettings, Error> {
    if is_pinned(config) {
        return Ok(FileSettings {
            upload_size_limit_mb: config.marti.upload_size_limit_mb,
            from_config_file: true,
        });
    }

    let stored = db.settings().get::<FileSettings>(SETTINGS_KEY).await?;

    Ok(FileSettings {
        upload_size_limit_mb: stored
            .map(|settings| settings.upload_size_limit_mb)
            .unwrap_or(config.marti.upload_size_limit_mb),
        from_config_file: false,
    })
}

/// The ceiling in megabytes, which is what every upload path wants.
///
/// # Errors
///
/// As [`resolve`].
pub async fn limit_mb(config: &Config, db: &Database) -> Result<u32, Error> {
    Ok(resolve(config, db).await?.upload_size_limit_mb)
}

/// The same ceiling in bytes, as the readers bound their streams with.
///
/// # Errors
///
/// As [`resolve`].
pub async fn limit_bytes(config: &Config, db: &Database) -> Result<u64, Error> {
    Ok(u64::from(limit_mb(config, db).await?) * 1_000_000)
}

/// Whether the configuration file has expressed a preference.
pub fn is_pinned(config: &Config) -> bool {
    config.marti.upload_size_limit_mb != MartiConfig::default().upload_size_limit_mb
}

/// Records a new ceiling.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for a limit of zero or one past
/// [`MAX_LIMIT_MB`], and a [`human_errors::Kind::System`] error if the write
/// fails.
pub async fn save(
    db: &Database,
    limit_mb: u32,
    by: Option<&Username>,
) -> Result<FileSettings, Error> {
    if limit_mb == 0 || limit_mb > MAX_LIMIT_MB {
        return Err(human_errors::user(
            format!("An upload limit has to be between 1 and {MAX_LIMIT_MB} MB."),
            &["Enter the largest data package this server should accept, in megabytes."],
        ));
    }

    let settings = FileSettings {
        upload_size_limit_mb: limit_mb,
        from_config_file: false,
    };

    db.settings().set(SETTINGS_KEY, settings, by).await?;

    info!(limit_mb, "Changed the upload size limit.");

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn an_installation_that_has_been_told_nothing_reports_the_default() {
        let resolved = resolve(&Config::default(), &db().await).await.unwrap();

        assert_eq!(
            resolved.upload_size_limit_mb,
            MartiConfig::default().upload_size_limit_mb,
        );
        assert!(!resolved.from_config_file);
    }

    #[tokio::test]
    async fn a_stored_limit_is_what_every_reader_then_sees() {
        let db = db().await;

        save(&db, 25, None).await.unwrap();

        assert_eq!(limit_mb(&Config::default(), &db).await.unwrap(), 25);
        assert_eq!(
            limit_bytes(&Config::default(), &db).await.unwrap(),
            25_000_000,
        );
    }

    #[tokio::test]
    async fn the_configuration_file_wins_and_reports_that_it_did() {
        // A value in a file an operator deploys is not something a web form
        // gets to replace, and a page that could not tell would offer an edit
        // that does not stick.
        let db = db().await;
        save(&db, 25, None).await.unwrap();

        let mut config = Config::default();
        config.marti.upload_size_limit_mb = 100;

        let resolved = resolve(&config, &db).await.unwrap();

        assert_eq!(resolved.upload_size_limit_mb, 100);
        assert!(resolved.from_config_file);
        assert!(is_pinned(&config));
    }

    #[tokio::test]
    async fn a_file_that_repeats_the_default_has_not_expressed_a_preference() {
        let db = db().await;
        save(&db, 25, None).await.unwrap();

        let mut config = Config::default();
        config.marti.upload_size_limit_mb = MartiConfig::default().upload_size_limit_mb;

        assert_eq!(
            resolve(&config, &db).await.unwrap().upload_size_limit_mb,
            25
        );
    }

    #[tokio::test]
    async fn a_limit_of_zero_or_an_absurd_one_is_refused() {
        let db = db().await;

        assert!(save(&db, 0, None).await.is_err());
        assert!(save(&db, MAX_LIMIT_MB + 1, None).await.is_err());
        assert!(save(&db, MAX_LIMIT_MB, None).await.is_ok());
    }
}
