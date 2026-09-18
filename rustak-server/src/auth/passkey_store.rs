//! Where a passkey lives between ceremonies, and where a ceremony lives between
//! its two halves.
//!
//! # Why the whole credential is stored
//!
//! `webauthn-rs` verifies an assertion against a [`Passkey`], which carries the
//! public key, the signature counter, the backup flags, the registered
//! extensions and the attestation it was registered with. Rebuilding one from
//! the columns by hand would mean reconstructing all of that, and getting any
//! of it wrong is a verification that quietly stops meaning what it should. So
//! the serialised credential is what goes in `passkeys.public_key`, and the
//! other columns mirror the parts a person reads in the UI or the database is
//! asked to index by.
//!
//! # Why a ceremony's state is on the server
//!
//! The challenge is the whole of what makes an assertion fresh. A client that
//! held its own state could replay one; a server that forgot its state could be
//! made to accept any. It therefore goes in the database, keyed by a handle the
//! client carries, is good for one attempt, and expires in minutes.

use chrono::{DateTime, Duration, Utc};
use rustak_core::prelude::*;
use webauthn_rs::prelude::{Credential, Passkey};

use crate::db::{
    Database, KeyValueStore as _,
    repos::{NewPasskey, PasskeyRow},
};

use super::setup::AUTH_STATE_PARTITION;

/// The prefix a ceremony's record is keyed by.
const CEREMONY_PREFIX: &str = "ceremony:";

/// How long a ceremony may stand unfinished.
///
/// The specification's own authenticator timeout is five minutes, so a shorter
/// window here would expire challenges the browser is still showing a prompt
/// for.
pub const CEREMONY_TTL_MINUTES: i64 = 5;

/// How many random bytes a challenge handle carries.
const HANDLE_BYTES: usize = 24;

/// What a ceremony is for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CeremonyKind {
    /// Registering a passkey for an account that is already known.
    Register {
        /// Whose passkey this will be.
        user_id: UserId,
        /// What it will be called.
        label: String,
        /// Whether the ceremony was authorised by the first-run registration
        /// token rather than by a session. The wizard is signed in with the
        /// result, so the two paths answer differently.
        bootstrap: bool,
    },
    /// Signing in as a named account.
    Login {
        /// Whose passkeys the challenge lists.
        user_id: UserId,
    },
    /// Signing in without naming an account first, which is what lets a
    /// browser offer a passkey without being told who it belongs to — and what
    /// stops the sign-in page being a way to ask which accounts exist.
    Discover,
}

/// A ceremony waiting for the browser to come back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ceremony {
    /// What it is for.
    pub kind: CeremonyKind,
    /// The `webauthn-rs` state, serialised.
    pub state: serde_json::Value,
    /// When it stops being valid.
    pub expires_at: DateTime<Utc>,
}

/// Records a ceremony and returns the handle the browser carries.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the state cannot be serialised
/// or stored.
pub async fn begin<T: Serialize>(
    db: &Database,
    kind: CeremonyKind,
    state: &T,
) -> Result<String, Error> {
    let handle = handle();

    db.set(
        AUTH_STATE_PARTITION,
        format!("{CEREMONY_PREFIX}{handle}"),
        Ceremony {
            kind,
            state: serde_json::to_value(state).or_system_err(&[
                "This is unexpected; please report it with the surrounding log entries.",
            ])?,
            expires_at: Utc::now() + Duration::minutes(CEREMONY_TTL_MINUTES),
        },
    )
    .await?;

    Ok(handle)
}

/// Spends a ceremony, whether or not it had expired.
///
/// One shot: the record is removed before its expiry is looked at, so a handle
/// cannot be presented twice even in the window before a sweep would have taken
/// it away.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the handle is unknown or the
/// ceremony has expired, and a [`human_errors::Kind::System`] error if a read
/// or write fails.
pub async fn claim(db: &Database, handle: &str) -> Result<Ceremony, Error> {
    let key = format!("{CEREMONY_PREFIX}{handle}");

    let Some(ceremony) = db
        .get::<Ceremony>(AUTH_STATE_PARTITION, key.clone())
        .await?
    else {
        return Err(expired());
    };

    db.remove(AUTH_STATE_PARTITION, key).await?;

    if ceremony.expires_at <= Utc::now() {
        return Err(expired());
    }

    Ok(ceremony)
}

/// Removes every ceremony that has expired.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or write fails.
pub async fn sweep(db: &Database) -> Result<usize, Error> {
    let now = Utc::now();
    let stored: Vec<(String, Ceremony)> = db.list(AUTH_STATE_PARTITION).await?;
    let mut removed = 0;

    for (key, ceremony) in stored {
        if key.starts_with(CEREMONY_PREFIX) && ceremony.expires_at <= now {
            db.remove(AUTH_STATE_PARTITION, key).await?;
            removed += 1;
        }
    }

    Ok(removed)
}

/// The stable identifier an authenticator stores alongside a passkey.
///
/// Derived from the account's row identifier rather than stored beside it: the
/// mapping has to be stable for the life of the account, and a column that
/// could drift out of step with the rows already in authenticators is a column
/// that eventually will.
pub fn user_handle(user_id: UserId) -> uuid::Uuid {
    uuid::Uuid::from_u128(user_id.get() as u128)
}

/// Turns a stored row back into something `webauthn-rs` can verify against.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the stored credential cannot be
/// read, which means it was written by a version that stored it differently.
pub fn to_passkey(row: &PasskeyRow) -> Result<Passkey, Error> {
    serde_json::from_slice(&row.public_key).or_system_err(&[
        "A stored passkey could not be read; it may have been written by a different version of rustak.",
    ])
}

/// Turns a freshly registered credential into a row.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the credential cannot be
/// serialised.
pub fn to_row(user_id: UserId, passkey: &Passkey, label: String) -> Result<NewPasskey, Error> {
    let public_key = serde_json::to_vec(passkey).or_system_err(&[
        "This is unexpected; please report it with the surrounding log entries.",
    ])?;

    let credential: Credential = passkey.clone().into();

    Ok(NewPasskey {
        user_id,
        credential_id: credential.cred_id.to_vec(),
        public_key,
        sign_count: credential.counter,
        transports: credential.transports.as_ref().map(|transports| {
            transports
                .iter()
                .map(|transport| format!("{transport:?}").to_ascii_lowercase())
                .collect()
        }),
        label,
        backup_eligible: credential.backup_eligible,
        backup_state: credential.backup_state,
    })
}

/// A random handle for a ceremony.
fn handle() -> String {
    use base64::Engine as _;
    use rand::Rng as _;

    let mut bytes = [0u8; HANDLE_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The one thing an unusable ceremony handle is ever told.
fn expired() -> Error {
    human_errors::user(
        "That took too long, so the request has expired.",
        &["Start again; a passkey prompt is only good for a few minutes."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn a_ceremony_is_good_for_exactly_one_attempt() {
        let db = database().await;
        let handle = begin(
            &db,
            CeremonyKind::Discover,
            &serde_json::json!({ "challenge": "abc" }),
        )
        .await
        .unwrap();

        let claimed = claim(&db, &handle).await.unwrap();
        assert!(matches!(claimed.kind, CeremonyKind::Discover));

        assert!(
            claim(&db, &handle).await.is_err(),
            "a handle that could be presented twice is a replay",
        );
    }

    #[tokio::test]
    async fn an_unknown_handle_and_an_expired_one_are_refused_the_same_way() {
        let db = database().await;

        let unknown = claim(&db, "not-a-handle").await.unwrap_err();

        let handle = begin(&db, CeremonyKind::Discover, &serde_json::json!({}))
            .await
            .unwrap();

        // Reach past the API to age it, which is the only thing a test can do
        // about a five-minute window.
        let key = format!("{CEREMONY_PREFIX}{handle}");
        let mut stored: Ceremony = db
            .get(AUTH_STATE_PARTITION, key.clone())
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        db.set(AUTH_STATE_PARTITION, key, stored).await.unwrap();

        let expired = claim(&db, &handle).await.unwrap_err();

        assert_eq!(unknown.description(), expired.description());
    }

    #[tokio::test]
    async fn sweeping_takes_away_the_ceremonies_nobody_finished() {
        let db = database().await;
        let live = begin(&db, CeremonyKind::Discover, &serde_json::json!({}))
            .await
            .unwrap();
        let stale = begin(&db, CeremonyKind::Discover, &serde_json::json!({}))
            .await
            .unwrap();

        let key = format!("{CEREMONY_PREFIX}{stale}");
        let mut stored: Ceremony = db
            .get(AUTH_STATE_PARTITION, key.clone())
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        db.set(AUTH_STATE_PARTITION, key, stored).await.unwrap();

        assert_eq!(sweep(&db).await.unwrap(), 1);
        assert!(claim(&db, &live).await.is_ok());
    }

    #[test]
    fn an_accounts_handle_is_the_same_every_time_it_is_asked_for() {
        assert_eq!(user_handle(UserId::new(7)), user_handle(UserId::new(7)));
        assert_ne!(user_handle(UserId::new(7)), user_handle(UserId::new(8)));
    }

    #[test]
    fn a_handle_is_random_and_survives_a_url() {
        let first = handle();

        assert_ne!(first, handle());
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }
}
