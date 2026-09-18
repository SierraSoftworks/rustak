//! Loading a rustak configuration file.
//!
//! Every rustak binary — the server, a sidecar, the test harness — reads a TOML
//! file the same way:
//!
//! 1. an optional environment file is loaded first, *overriding* the process
//!    environment ([`load_env_file`]);
//! 2. the TOML text is read and every `${{ env.NAME }}` expression in it is
//!    substituted ([`interpolation`] finds them, [`mod@env`] evaluates them);
//! 3. the result is parsed into the caller's own `#[serde(deny_unknown_fields)]`
//!    types ([`load`]).
//!
//! Interpolation happens **before** parsing rather than after, which is what
//! lets a secret be injected anywhere a value can appear — inside a string, a
//! number, or a whole inline table — without the schema having to know that any
//! particular key might be a template.
//!
//! # Secrets belong in the environment, not in the file
//!
//! The file is the thing an operator commits, shares in a support ticket and
//! bind-mounts into a container. Anything that must not be in it is written as
//! `${{ env.RUSTAK_SOMETHING }}` and supplied separately. An environment
//! variable that is not set leaves its marker in place rather than becoming an
//! empty string, so a missing secret is reported by whoever needed it instead of
//! silently disabling the protection it was for — see [`mod@env`] for why.
//!
//! # Example
//!
//! ```
//! # use serde::Deserialize;
//! #[derive(Debug, Deserialize)]
//! #[serde(deny_unknown_fields)]
//! struct Auth {
//!     client_id: String,
//!     client_secret: String,
//! }
//!
//! let auth: Auth = rustak_core::config::load_str(
//!     r#"
//!     client_id = "rustak"
//!     client_secret = "${{ env.RUSTAK_DOC_CLIENT_SECRET }}"
//!     "#,
//! )
//! .unwrap();
//!
//! assert_eq!(auth.client_id, "rustak");
//!
//! // Nothing set `RUSTAK_DOC_CLIENT_SECRET`, so the marker survives parsing
//! // and the code that needs the secret can refuse it by name — rather than
//! // the server starting up with a blank client secret.
//! assert!(rustak_core::config::env::is_unresolved(&auth.client_secret));
//! ```

pub mod duration;
pub mod env;
pub mod interpolation;
pub mod listen;

use std::path::{Path, PathBuf};

use human_errors::ResultExt;
use serde::de::DeserializeOwned;

pub use interpolation::interpolate;
pub use listen::ListenAddr;

/// Advice for the several ways a file can fail to be readable.
const ADVICE_UNREADABLE: &[&str] = &[
    "Ensure the file exists and is readable.",
    "Check that you have the necessary permissions to read the file.",
];

/// Advice for a file we read but could not make sense of.
const ADVICE_INVALID_TOML: &[&str] = &[
    "Ensure that the file is valid TOML.",
    "Check the key against `config.example.toml`, which documents every option.",
];

/// Reads, interpolates and parses a configuration file.
///
/// # Errors
///
/// Returns a [`human_errors::Kind::User`] error when the file cannot be read,
/// contains an interpolation expression we do not understand, or does not parse
/// as `T`.
pub fn load<T: DeserializeOwned>(path: impl Into<PathBuf>) -> Result<T, human_errors::Error> {
    let path = path.into();

    let contents = std::fs::read_to_string(&path).wrap_user_err(
        format!("We could not read your config file '{}'.", path.display()),
        ADVICE_UNREADABLE,
    )?;

    load_str(&contents).wrap_user_err(
        format!("We could not load your config file '{}'.", path.display()),
        ADVICE_INVALID_TOML,
    )
}

/// Interpolates and parses configuration text that is already in hand.
///
/// This is what [`load`] does once the file has been read, and what the test
/// suites and the `config.example.toml` schema test use directly.
///
/// # Errors
///
/// Returns a [`human_errors::Kind::User`] error when the text contains an
/// interpolation expression we do not understand, or does not parse as `T`.
pub fn load_str<T: DeserializeOwned>(contents: &str) -> Result<T, human_errors::Error> {
    let contents = interpolate(contents, env::resolve)?;

    toml::from_str(&contents).wrap_user_err(
        "Your configuration file could not be loaded.",
        ADVICE_INVALID_TOML,
    )
}

/// Loads an environment file, if there is one, over the process environment.
///
/// Values in the file **override** variables already set in the process, which
/// is what makes a local `.env` useful for development: it is the more specific
/// statement of intent.
///
/// Absence is not an error — `--env` defaults to `.env`, and most installations
/// do not have one.
///
/// # Anything that is not a regular file is skipped
///
/// A path that exists but is a directory, a device, or — the case this guard was
/// written for — a **named pipe** is ignored with a warning rather than opened.
/// Reading a FIFO blocks until something writes to it, so opening one here would
/// hang the process before telemetry is even up, with no log line to say why.
/// automate hit exactly that and had to work around it in its e2e script; a
/// `.env` that is not a file is never a `.env` we can use, so we decline it and
/// carry on.
///
/// # Errors
///
/// Returns a [`human_errors::Kind::User`] error when the path exists, is a
/// regular file, and cannot be read or parsed as `KEY=value` lines.
pub fn load_env_file(path: impl AsRef<Path>) -> Result<(), human_errors::Error> {
    let path = path.as_ref();

    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        // Absence is the common case, not a failure.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(human_errors::user(
                format!(
                    "We could not inspect your environment file '{}': {err}",
                    path.display()
                ),
                ADVICE_UNREADABLE,
            ));
        }
    };

    if !metadata.is_file() {
        // `metadata` follows symlinks and does not open the path, so asking
        // this question is safe even when the answer is "a FIFO nobody is
        // writing to".
        tracing::warn!(
            path = %path.display(),
            "Ignoring the environment file because it is not a regular file.",
        );
        return Ok(());
    }

    dotenvy::from_path_override(path).wrap_user_err(
        format!(
            "We could not load your environment file '{}'.",
            path.display()
        ),
        &[
            "Ensure the file is in the correct .env format (KEY=value).",
            "Check that you have the necessary permissions to read the file.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Section {
        name: String,
        #[serde(default)]
        listen: Vec<ListenAddr>,
    }

    #[test]
    fn a_file_is_read_interpolated_and_parsed_in_one_step() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "name = \"rustak\"\nlisten = [\":8446\"]\n").unwrap();

        let section: Section = load(&path).unwrap();

        assert_eq!(section.name, "rustak");
        assert_eq!(section.listen, vec![":8446".parse().unwrap()]);
    }

    #[test]
    fn a_missing_file_names_the_path_that_was_missing() {
        let Err(err) = load::<Section>("/rustak/definitely/not/here.toml") else {
            panic!("a missing config file should not load");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("not/here.toml"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_refused_so_the_example_file_is_a_real_schema() {
        // `deny_unknown_fields` is what makes `config.example.toml` a test
        // rather than documentation that drifts. Assert that the loader does
        // not undo it (by, say, parsing into a `toml::Value` first).
        let Err(err) = load_str::<Section>("name = \"rustak\"\nlissen = [\":8446\"]\n") else {
            panic!("a misspelled key should not load");
        };

        assert!(err.to_string().contains("lissen"), "{err}");
    }

    #[test]
    fn interpolation_happens_before_parsing_so_a_secret_can_be_any_value() {
        // Substituting after the parse would confine templates to string
        // fields. `PATH` is set everywhere, so this needs nothing arranged.
        let expected = std::env::var("PATH").unwrap();

        let section: Section = load_str("name = \"${{ env.PATH }}\"").unwrap();

        assert_eq!(section.name, expected);
    }

    #[test]
    fn an_expression_we_do_not_understand_stops_the_load() {
        let Err(err) = load_str::<Section>("name = \"${{ secrets.NAME }}\"") else {
            panic!("an unknown expression should not load");
        };

        assert!(err.to_string().contains("secrets.NAME"), "{err}");
    }

    #[test]
    fn an_absent_environment_file_is_not_an_error() {
        // `--env` defaults to `.env` and most installations do not have one, so
        // absence has to be the quiet path.
        load_env_file("/rustak/definitely/not/here.env").unwrap();
    }

    #[test]
    fn a_directory_in_place_of_an_environment_file_is_skipped_rather_than_opened() {
        let directory = tempfile::tempdir().unwrap();

        load_env_file(directory.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_named_pipe_in_place_of_an_environment_file_is_skipped_rather_than_opened() {
        // The guard this test exists for: opening a FIFO blocks until somebody
        // writes to it, so without the `is_file` check this call would hang the
        // process before telemetry is up — which is precisely the failure
        // automate's e2e script had to work around. Nothing writes to this
        // pipe, so a regression hangs the test suite rather than failing it,
        // which is the loudest signal available short of a watchdog thread.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("blocking.env");

        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo should be available on a unix host");
        assert!(made.success(), "mkfifo failed for {}", path.display());

        load_env_file(&path).unwrap();
    }

    #[test]
    fn a_real_environment_file_is_loaded_over_the_process_environment() {
        // One test does the loading, because `dotenvy` mutates the process
        // environment and two of these running at once would race.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("real.env");
        std::fs::write(&path, "RUSTAK_CONFIG_DOTENV_TEST=loaded\n").unwrap();

        load_env_file(&path).unwrap();

        assert_eq!(
            std::env::var("RUSTAK_CONFIG_DOTENV_TEST").as_deref(),
            Ok("loaded"),
        );
    }

    #[test]
    fn a_malformed_environment_file_is_reported_rather_than_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.env");
        std::fs::write(&path, "this is not a key=value line\nand nor is this\n").unwrap();

        let Err(err) = load_env_file(&path) else {
            panic!("a malformed environment file should be reported");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("broken.env"), "{err}");
    }
}
