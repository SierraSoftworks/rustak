//! The two one-time tokens the first run depends on.
//!
//! A fresh installation has nobody to authenticate as: there are no local
//! passwords, and the first administrator's passkey is the thing being created.
//! Something has to stand in, and it has to be something only whoever can read
//! the server's own filesystem holds.
//!
//! - The **setup token** is written to `[auth] setup_token_file` with mode
//!   `0600` on the first start of an installation with no administrator, and
//!   logged once. It authorises exactly one thing: creating that administrator.
//! - The **registration token** is handed back by that call and authorises
//!   exactly one more: registering the new administrator's first passkey. It
//!   lives for minutes, in the database, and is spent on use.
//!
//! Both are 256 bits of randomness stored as a SHA-256, compared in constant
//! time, and neither survives the wizard: [`consume`] removes the record and
//! deletes the file, and after that `/api/v1/setup/*` answers `410`.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

use crate::db::Database;
use crate::prelude::*;

/// The key/value partition the short-lived authentication state lives in.
pub const AUTH_STATE_PARTITION: &str = "auth-state";

/// The key the setup token's record lives under.
const SETUP_TOKEN_KEY: &str = "setup-token";

/// The prefix a registration token's record is keyed by.
const REGISTRATION_PREFIX: &str = "registration:";

/// How many random bytes either token carries.
const TOKEN_BYTES: usize = 32;

/// How long a registration token lasts.
///
/// Long enough to walk from one wizard step to a browser's passkey prompt,
/// short enough that a token left in a log or a screenshot is worthless by the
/// time anybody reads it.
pub const REGISTRATION_TTL_MINUTES: i64 = 10;

/// Base64 as the tokens use it: URL-safe, unpadded, so it can be typed and
/// pasted without escaping.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// What is stored for the setup token.
#[derive(Clone, Serialize, Deserialize)]
struct SetupRecord {
    hash: String,
    created_at: DateTime<Utc>,
}

/// What is stored for a registration token.
#[derive(Clone, Serialize, Deserialize)]
struct RegistrationRecord {
    user_id: UserId,
    expires_at: DateTime<Utc>,
}

/// A freshly written setup token, so start-up can log where it went.
pub struct SetupToken {
    /// The token itself. Held only long enough to be written and logged.
    pub token: String,
    /// Where it was written.
    pub path: PathBuf,
}

impl std::fmt::Debug for SetupToken {
    /// Written out so that the token cannot reach a log through a `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupToken")
            .field("token", &"***")
            .field("path", &self.path)
            .finish()
    }
}

/// Whether the first-run wizard still applies.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if either read fails.
pub async fn required(db: &Database) -> Result<bool, Error> {
    if crate::identity::settings::stored(db)
        .await?
        .is_setup_complete()
    {
        return Ok(false);
    }

    Ok(db.users().count_admins().await? == 0)
}

/// Writes a setup token on the first start of an installation with no
/// administrator, and returns it so start-up can log where it is.
///
/// Returns [`None`] when the wizard does not apply, and when a token has
/// already been written — restarting the server does not invalidate the token
/// somebody is halfway through typing.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the file cannot be written, and a
/// [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("auth.setup.ensure", skip_all, err(Display))]
pub async fn ensure(db: &Database, path: &Path) -> Result<Option<SetupToken>, Error> {
    if !required(db).await? {
        return Ok(None);
    }

    if db
        .get::<SetupRecord>(AUTH_STATE_PARTITION, SETUP_TOKEN_KEY)
        .await?
        .is_some()
        && tokio::fs::try_exists(path).await.unwrap_or(false)
    {
        return Ok(None);
    }

    let token = random_token();

    db.set(
        AUTH_STATE_PARTITION,
        SETUP_TOKEN_KEY,
        SetupRecord {
            hash: digest(&token),
            created_at: Utc::now(),
        },
    )
    .await?;

    write_private(path, &token).await?;

    Ok(Some(SetupToken {
        token,
        path: path.to_path_buf(),
    }))
}

/// Checks a presented setup token.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when there is no token to present or
/// the one presented is not it, and a [`human_errors::Kind::System`] error if
/// the read fails.
pub async fn verify(db: &Database, presented: &str) -> Result<(), Error> {
    let Some(record) = db
        .get::<SetupRecord>(AUTH_STATE_PARTITION, SETUP_TOKEN_KEY)
        .await?
    else {
        return Err(refused());
    };

    if !constant_time_eq(&digest(presented), &record.hash) {
        return Err(refused());
    }

    Ok(())
}

/// Removes the setup token and deletes its file.
///
/// Called when the wizard completes. A file that cannot be deleted is a warning
/// rather than a failure: the record is gone, so the token is already useless,
/// and refusing to finish setup over it would be worse.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the record cannot be removed.
#[instrument("auth.setup.consume", skip_all, err(Display))]
pub async fn consume(db: &Database, path: &Path) -> Result<(), Error> {
    db.remove(AUTH_STATE_PARTITION, SETUP_TOKEN_KEY).await?;

    match tokio::fs::remove_file(path).await {
        Ok(()) => info!(path = %path.display(), "Deleted the first-run setup token."),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!(
            path = %path.display(),
            error = %err,
            "The setup token file could not be deleted; it no longer authorises anything."
        ),
    }

    Ok(())
}

/// Mints a token authorising one passkey registration for one account.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
pub async fn issue_registration(db: &Database, user_id: UserId) -> Result<String, Error> {
    let token = random_token();

    db.set(
        AUTH_STATE_PARTITION,
        format!("{REGISTRATION_PREFIX}{}", digest(&token)),
        RegistrationRecord {
            user_id,
            expires_at: Utc::now() + Duration::minutes(REGISTRATION_TTL_MINUTES),
        },
    )
    .await?;

    Ok(token)
}

/// Spends a registration token, returning the account it authorises.
///
/// One shot: the record is removed whether or not it had expired, so a token
/// cannot be replayed even in the window before a sweep would have removed it.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the token is unknown or expired,
/// and a [`human_errors::Kind::System`] error if a read or write fails.
pub async fn claim_registration(db: &Database, presented: &str) -> Result<UserId, Error> {
    let key = format!("{REGISTRATION_PREFIX}{}", digest(presented));

    let Some(record) = db
        .get::<RegistrationRecord>(AUTH_STATE_PARTITION, key.clone())
        .await?
    else {
        return Err(registration_refused());
    };

    db.remove(AUTH_STATE_PARTITION, key).await?;

    if record.expires_at <= Utc::now() {
        return Err(registration_refused());
    }

    Ok(record.user_id)
}

/// 256 bits of randomness, URL-safe.
fn random_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    B64.encode(bytes)
}

/// The stored form of a token.
fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Compares two digests without leaking where they first differ.
///
/// The values compared are hashes of high-entropy tokens, so a timing signal
/// would be hard to use — but "hard to use" is not a property worth relying on
/// when the alternative is four lines.
fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |differences, (left, right)| {
            differences | (left ^ right)
        })
        == 0
}

/// Writes a file only its owner can read.
async fn write_private(path: &Path, contents: &str) -> Result<(), Error> {
    let advice: &[&str] = &[
        "Check that the directory in [auth] setup_token_file exists and is writable.",
        "Set [auth] setup_token_file to a path this process can write to.",
    ];

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.wrap_user_err(
            format!("We could not create the directory for {}.", path.display()),
            advice,
        )?;
    }

    tokio::fs::write(path, contents).await.wrap_user_err(
        format!("We could not write the setup token to {}.", path.display()),
        advice,
    )?;

    restrict(path).await
}

/// Narrows a file to its owner, where the platform has the concept.
#[cfg(unix)]
async fn restrict(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt as _;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .wrap_user_err(
            format!("We could not restrict {} to this user.", path.display()),
            &["Check that the file is on a filesystem that supports permissions."],
        )
}

/// Windows has no mode bits; the file inherits the directory's access control.
#[cfg(not(unix))]
async fn restrict(_path: &Path) -> Result<(), Error> {
    Ok(())
}

/// The one thing a refused setup token is ever told.
fn refused() -> Error {
    human_errors::user(
        "That setup token is not the one this server is waiting for.",
        &[
            "Copy the token from the file named in the server's start-up log.",
            "If setup has already been completed, there is nothing left to set up.",
        ],
    )
}

/// The one thing a refused registration token is ever told.
fn registration_refused() -> Error {
    human_errors::user(
        "That registration has expired.",
        &["Start the setup wizard again to register the administrator's passkey."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    async fn admin(db: &Database) -> UserId {
        db.users()
            .create(NewUser {
                is_admin: true,
                ..NewUser::person(Username::parse("ada").unwrap())
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_fresh_installation_is_waiting_for_somebody_to_set_it_up() {
        let db = database().await;

        assert!(required(&db).await.unwrap());

        admin(&db).await;

        assert!(
            !required(&db).await.unwrap(),
            "an installation with an administrator has nothing left to bootstrap",
        );
    }

    #[tokio::test]
    async fn the_token_is_written_where_the_operator_was_told_to_look() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("setup-token");
        let db = database().await;

        let issued = ensure(&db, &path)
            .await
            .unwrap()
            .expect("a token is written");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), issued.token);
        assert!(verify(&db, &issued.token).await.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_readable_only_by_the_user_the_server_runs_as() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup-token");
        let db = database().await;

        ensure(&db, &path).await.unwrap().unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();

        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn restarting_does_not_invalidate_a_token_somebody_is_typing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup-token");
        let db = database().await;

        let first = ensure(&db, &path).await.unwrap().unwrap();

        assert!(ensure(&db, &path).await.unwrap().is_none());
        assert!(verify(&db, &first.token).await.is_ok());
    }

    #[tokio::test]
    async fn a_deleted_file_is_written_again_rather_than_leaving_nobody_a_way_in() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup-token");
        let db = database().await;

        let first = ensure(&db, &path).await.unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();

        let second = ensure(&db, &path).await.unwrap().expect("a fresh token");

        assert_ne!(first.token, second.token);
        assert!(verify(&db, &second.token).await.is_ok());
        assert!(verify(&db, &first.token).await.is_err());
    }

    #[tokio::test]
    async fn an_installation_with_an_administrator_writes_no_token() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup-token");
        let db = database().await;

        admin(&db).await;

        assert!(ensure(&db, &path).await.unwrap().is_none());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn the_wrong_token_and_no_token_are_refused_the_same_way() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup-token");
        let db = database().await;

        let missing = verify(&db, "anything").await.unwrap_err();

        ensure(&db, &path).await.unwrap();
        let wrong = verify(&db, "anything").await.unwrap_err();

        assert_eq!(missing.description(), wrong.description());
    }

    #[tokio::test]
    async fn completing_setup_takes_the_token_away() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup-token");
        let db = database().await;

        let issued = ensure(&db, &path).await.unwrap().unwrap();

        consume(&db, &path).await.unwrap();

        assert!(!path.exists());
        assert!(verify(&db, &issued.token).await.is_err());
    }

    #[tokio::test]
    async fn a_registration_token_authorises_one_account_once() {
        let db = database().await;
        let user = admin(&db).await;

        let token = issue_registration(&db, user).await.unwrap();

        assert_eq!(claim_registration(&db, &token).await.unwrap(), user);
        assert!(
            claim_registration(&db, &token).await.is_err(),
            "a token that could be spent twice would register two passkeys",
        );
    }

    #[tokio::test]
    async fn an_unknown_registration_token_is_refused() {
        let db = database().await;

        assert!(claim_registration(&db, "not-a-token").await.is_err());
    }

    #[test]
    fn a_digest_comparison_does_not_stop_at_the_first_difference() {
        assert!(constant_time_eq("abcd", "abcd"));
        assert!(!constant_time_eq("abcd", "abce"));
        assert!(!constant_time_eq("abcd", "abc"));
    }

    #[test]
    fn a_token_is_high_entropy_and_survives_being_pasted() {
        let token = random_token();

        assert!(token.len() >= 43);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(token, random_token());
    }

    #[test]
    fn a_setup_token_is_redacted_when_it_is_printed() {
        let rendered = format!(
            "{:?}",
            SetupToken {
                token: "the-secret".to_string(),
                path: PathBuf::from("/data/setup-token"),
            }
        );

        assert!(!rendered.contains("the-secret"));
        assert!(rendered.contains("/data/setup-token"));
    }
}
