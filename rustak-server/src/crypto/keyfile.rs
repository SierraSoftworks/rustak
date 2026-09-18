//! Where the generated encryption key lives, and how it is created and read.

use std::path::{Path, PathBuf};

use rustak_core::prelude::*;

use super::key::SecretKey;

/// The filename suffix of the generated key file, appended to the database path.
const KEY_FILE_SUFFIX: &str = ".key";

/// Resolves the key file that accompanies a given database.
pub fn key_file_for(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_owned();
    path.push(KEY_FILE_SUFFIX);

    PathBuf::from(path)
}

/// Loads the active key, generating and persisting one if none is configured.
///
/// Generating a key on first run rather than demanding configuration is what
/// lets an existing install upgrade without the operator having to do anything
/// first. The generated key lives beside the database rather than inside it,
/// so that a database backup is not by itself enough to read the secrets it
/// contains.
pub fn load_or_create_key(
    configured: Option<&str>,
    database: &Path,
) -> Result<SecretKey, human_errors::Error> {
    if let Some(configured) = configured.map(str::trim).filter(|k| !k.is_empty()) {
        return SecretKey::from_encoded(configured);
    }

    let path = key_file_for(database);

    if path.exists() {
        let contents = std::fs::read_to_string(&path).wrap_user_err(
            format!(
                "We could not read the encryption key at '{}'.",
                path.display()
            ),
            &[
                "Check that the file is readable by the user rustak runs as.",
                "Set 'secret_key' under [auth] to manage the key yourself instead.",
            ],
        )?;

        warn_if_world_readable(&path);

        return SecretKey::from_encoded(&contents);
    }

    let key = SecretKey::generate();
    write_key_file(&path, &key)?;

    warn!(
        key_file = %path.display(),
        key_id = %key.id(),
        "No encryption key was configured, so one has been generated. Back this file up: without it, stored secrets cannot be recovered."
    );

    Ok(key)
}

/// Writes a key file readable only by its owner.
fn write_key_file(path: &Path, key: &SecretKey) -> Result<(), human_errors::Error> {
    let contents = key.to_encoded();

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        // Created with restrictive permissions from the outset rather than
        // chmod-ed afterwards, which would leave a window where the key is
        // world-readable.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .wrap_user_err(
                format!(
                    "We could not create an encryption key at '{}'.",
                    path.display()
                ),
                &[
                    "Check that the directory exists and is writable by the user rustak runs as.",
                    "Set 'secret_key' under [auth] to manage the key yourself instead.",
                ],
            )?;

        file.write_all(contents.as_bytes()).wrap_user_err(
            format!(
                "We could not write the encryption key at '{}'.",
                path.display()
            ),
            &["Check that there is free space on the volume holding the key file."],
        )?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, contents.as_bytes()).wrap_user_err(
            format!(
                "We could not create an encryption key at '{}'.",
                path.display()
            ),
            &[
                "Check that the directory exists and is writable by the user rustak runs as.",
                "Set 'secret_key' under [auth] to manage the key yourself instead.",
            ],
        )?;
    }

    Ok(())
}

/// Warns when a key file is readable by users other than its owner.
fn warn_if_world_readable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.permissions().mode() & 0o077;

            if mode != 0 {
                warn!(
                    key_file = %path.display(),
                    "The encryption key file is readable by other users on this host. Restrict it with: chmod 600 {}",
                    path.display()
                );
            }
        }
    }

    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_key_is_preferred_over_generating_one() {
        let dir = std::env::temp_dir().join(format!("rustak-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("database.sqlite");

        let configured = SecretKey::generate();
        let loaded = load_or_create_key(Some(&configured.to_encoded()), &database).unwrap();

        assert_eq!(loaded.id(), configured.id());
        assert!(
            !key_file_for(&database).exists(),
            "a key file should not be created when one is configured"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_generated_key_is_persisted_and_reused_on_the_next_start() {
        let dir = std::env::temp_dir().join(format!("rustak-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("database.sqlite");

        let first = load_or_create_key(None, &database).unwrap();
        let key_file = key_file_for(&database);
        assert!(key_file.exists());

        // Restarting must not invalidate everything sealed by the last run.
        let second = load_or_create_key(None, &database).unwrap();
        assert_eq!(first.id(), second.id());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the key file must not be readable by other users"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_configured_key_falls_back_to_the_key_file() {
        // Distinguishes "explicitly blank" from "absent"; an operator whose
        // environment variable expands to nothing should not be left without a
        // working install.
        let dir = std::env::temp_dir().join(format!("rustak-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("database.sqlite");

        let key = load_or_create_key(Some("  "), &database).unwrap();
        assert_eq!(load_or_create_key(None, &database).unwrap().id(), key.id());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_key_file_sits_beside_the_database() {
        assert_eq!(
            key_file_for(Path::new("/var/lib/rustak/database.sqlite")),
            PathBuf::from("/var/lib/rustak/database.sqlite.key")
        );
    }
}
