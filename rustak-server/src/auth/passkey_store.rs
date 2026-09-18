//! Where a passkey lives between ceremonies, and where a ceremony lives between
//! its two halves.
//!
//! # What is stored for a credential, and why
//!
//! `webauthn_rp` splits a registered credential into a `StaticState` (the
//! public key and the registration-time extension outputs, which never change)
//! and a [`DynamicState`] (the signature counter, the backup flags and whether
//! the user has ever been verified, which move with each sign-in). The static
//! half is what goes in `passkeys.public_key`, in the library's own binary
//! encoding; the dynamic half is rebuilt from the columns that already carry
//! it, so that the counter a human reads in the database is the counter the
//! check uses rather than a second copy that can drift out of step with it.
//!
//! `user_verified` is not a column because it cannot be false here: both
//! ceremonies demand `userVerification: "required"`, so a credential that
//! reached this table was verified. `authenticator_attachment` is not stored
//! either — it says *how* the browser reached the authenticator, it carries no
//! authentication weight, and the sign-in check is told to ignore it.
//!
//! # Why a ceremony's state is on the server
//!
//! The challenge is the whole of what makes an assertion fresh. A client that
//! held its own state could replay one; a server that forgot its state could be
//! made to accept any. It therefore goes in the database, keyed by a handle the
//! client carries, is good for one attempt, and expires in minutes.
//!
//! The library's own timeout travels inside the encoded state and is enforced
//! by it; [`CEREMONY_TTL_MINUTES`] is the outer sweep, and the two are the same
//! five minutes on purpose.

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use rustak_core::prelude::*;
use webauthn_rp::RegisteredCredential;
use webauthn_rp::bin::Encode as _;
use webauthn_rp::request::register::UserHandle16;
use webauthn_rp::response::register::{CompressedPubKey, DynamicState, StaticState};
use webauthn_rp::response::{
    AuthTransports, AuthenticatorAttachment, AuthenticatorTransport, Backup, CredentialId,
};

use crate::db::{
    Database, KeyValueStore as _,
    repos::{NewPasskey, PasskeyRow},
};

/// The key/value partition a half-finished ceremony lives in.
///
/// Its own, rather than shared with the setup tokens and the pending sign-ins:
/// a `list` over a partition deserialises every row into one type, so one row
/// of another shape aborted the sweep below entirely (R-01 M8).
pub const CEREMONY_PARTITION: &str = "auth-ceremony";

/// How many bytes the user handle an authenticator stores is made of.
pub const USER_HANDLE_LEN: usize = 16;

/// The public key of a stored credential, in the shape the verifier wants.
///
/// The array sizes are the ones `webauthn_rp`'s decoder produces: an Ed25519
/// point, a compressed P-256 point, a compressed P-384 point, and an RSA
/// modulus whose length is not fixed.
pub type StoredPubKey = CompressedPubKey<[u8; 32], [u8; 32], [u8; 48], Vec<u8>>;

/// Base64, because the ceremony record is JSON.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

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
    /// The library's own ceremony state, base64url of its binary encoding.
    pub state: String,
    /// The relying-party identifier this ceremony was started under.
    ///
    /// Recorded so that the finish can refuse a ceremony started under one host
    /// name and completed under another: the `Passkeys` verifier is rebuilt per
    /// request, and where no base URL is configured it was derived from the
    /// caller-supplied `Host` header — so the caller supplied the value the
    /// origin check compared against (R-01 M10). Empty only in a record written
    /// by an older build, which the finish refuses.
    #[serde(default)]
    pub rp_id: String,
    /// When it stops being valid.
    pub expires_at: DateTime<Utc>,
}

/// Records a ceremony and returns the handle the browser carries.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the state cannot be stored.
pub async fn begin(
    db: &Database,
    kind: CeremonyKind,
    rp_id: &str,
    state: &[u8],
) -> Result<String, Error> {
    let handle = handle();

    db.set(
        CEREMONY_PARTITION,
        format!("{CEREMONY_PREFIX}{handle}"),
        Ceremony {
            kind,
            state: B64.encode(state),
            rp_id: rp_id.to_string(),
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

    // The read *is* the delete, in one write transaction: N parallel posts of a
    // captured `{challenge_id, credential}` pair used to yield N sessions
    // (R-01 M13).
    let Some(ceremony) = db.take::<Ceremony>(CEREMONY_PARTITION, key).await? else {
        return Err(expired());
    };

    if ceremony.expires_at <= Utc::now() {
        return Err(expired());
    }

    Ok(ceremony)
}

/// Refuses a ceremony that is being finished under a different relying party.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error — the same one an unknown handle gets,
/// so the mismatch is not an oracle for which host names this server answers on.
pub fn require_rp_id(ceremony: &Ceremony, rp_id: &str) -> Result<(), Error> {
    if !ceremony.rp_id.is_empty() && ceremony.rp_id == rp_id {
        return Ok(());
    }

    warn!(
        expected = %ceremony.rp_id,
        "Refused a passkey ceremony finished under a different relying party."
    );

    Err(expired())
}

/// The library state a claimed ceremony was stored with.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when it cannot be read, which means
/// it was written by a version that stored it differently.
pub fn state_of(ceremony: &Ceremony) -> Result<Vec<u8>, Error> {
    B64.decode(&ceremony.state)
        .map_err(|_| unreadable("A stored ceremony"))
}

/// Removes every ceremony that has expired.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or write fails.
pub async fn sweep(db: &Database) -> Result<usize, Error> {
    let now = Utc::now();
    let stored: Vec<(String, Ceremony)> = db.list(CEREMONY_PARTITION).await?;
    let mut removed = 0;

    for (key, ceremony) in stored {
        if key.starts_with(CEREMONY_PREFIX) && ceremony.expires_at <= now {
            db.remove(CEREMONY_PARTITION, key).await?;
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
/// that eventually will. Sixteen bytes rather than the specification's
/// recommended sixty-four, because the value is a UUID built from a row id and
/// padding it would not make it any less guessable.
pub fn user_handle(user_id: UserId) -> UserHandle16 {
    UserHandle16::from(*uuid::Uuid::from_u128(user_id.get() as u128).as_bytes())
}

/// The credential's immutable half, as the verifier wants it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the stored key cannot be read,
/// which means it was written by a version that stored it differently.
pub fn static_state_of(row: &PasskeyRow) -> Result<StaticState<StoredPubKey>, Error> {
    use webauthn_rp::bin::Decode as _;

    StaticState::<StoredPubKey>::decode(row.public_key.as_slice())
        .map_err(|_| unreadable("A stored passkey"))
}

/// The credential's mutable half, rebuilt from the columns that carry it.
///
/// `user_verified` is `true` because both ceremonies demand a verified user, so
/// a credential that reached this table was verified; a column that can only
/// hold one value would be storing nothing.
pub fn dynamic_state_of(row: &PasskeyRow) -> DynamicState {
    DynamicState {
        user_verified: true,
        backup: match (row.backup_eligible, row.backup_state) {
            (false, _) => Backup::NotEligible,
            (true, false) => Backup::Eligible,
            (true, true) => Backup::Exists,
        },
        sign_count: row.sign_count,
        authenticator_attachment: AuthenticatorAttachment::None,
    }
}

/// The credential identifier a stored row names.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the stored identifier is not one
/// WebAuthn allows, which means it was not written by this code.
pub fn credential_id_of(row: &PasskeyRow) -> Result<CredentialId<&[u8]>, Error> {
    CredentialId::try_from(row.credential_id.as_slice()).map_err(|_| unreadable("A stored passkey"))
}

/// The transports a stored row hints at.
///
/// An unknown name is dropped rather than refused: this is a hint the browser
/// uses to suggest how to reach the authenticator, and a transport invented
/// after this version shipped must not stop somebody signing in.
pub fn transports_of(row: &PasskeyRow) -> AuthTransports {
    row.transports.as_deref().unwrap_or_default().iter().fold(
        AuthTransports::NONE,
        |transports, name| match transport_of(name) {
            Some(transport) => transports.add(transport),
            None => transports,
        },
    )
}

/// Turns a freshly registered credential into a row.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the public key cannot be
/// encoded.
pub fn to_row(
    user_id: UserId,
    credential: &RegisteredCredential<'_, USER_HANDLE_LEN>,
    label: String,
) -> Result<NewPasskey, Error> {
    let (id, transports, _, static_state, dynamic_state, _) = credential.as_parts();

    let public_key = static_state.encode().or_system_err(&[
        "This is unexpected; please report it with the surrounding log entries.",
    ])?;

    Ok(NewPasskey {
        user_id,
        credential_id: id.as_ref().to_vec(),
        public_key,
        sign_count: dynamic_state.sign_count,
        transports: transport_names(transports),
        label,
        backup_eligible: !matches!(dynamic_state.backup, Backup::NotEligible),
        backup_state: matches!(dynamic_state.backup, Backup::Exists),
    })
}

/// The transport a stored name means.
fn transport_of(name: &str) -> Option<AuthenticatorTransport> {
    Some(match name {
        "ble" => AuthenticatorTransport::Ble,
        "hybrid" => AuthenticatorTransport::Hybrid,
        "internal" => AuthenticatorTransport::Internal,
        "nfc" => AuthenticatorTransport::Nfc,
        "smart-card" => AuthenticatorTransport::SmartCard,
        "usb" => AuthenticatorTransport::Usb,
        _ => return None,
    })
}

/// The names [`transports_of`] reads back.
fn transport_names(transports: AuthTransports) -> Option<Vec<String>> {
    let names: Vec<String> = ["ble", "hybrid", "internal", "nfc", "smart-card", "usb"]
        .into_iter()
        .filter(|name| transport_of(name).is_some_and(|t| transports.contains(t)))
        .map(str::to_string)
        .collect();

    Some(names).filter(|names| !names.is_empty())
}

/// A random handle for a ceremony.
fn handle() -> String {
    use rand::Rng as _;

    let mut bytes = [0u8; HANDLE_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    B64.encode(bytes)
}

/// The one thing an unusable ceremony handle is ever told.
fn expired() -> Error {
    human_errors::user(
        "That took too long, so the request has expired.",
        &["Start again; a passkey prompt is only good for a few minutes."],
    )
}

/// What a caller is told when something we wrote cannot be read back.
fn unreadable(what: &str) -> Error {
    human_errors::system(
        format!("{what} could not be read."),
        &["It may have been written by a different version of rustak."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relying party every ceremony here is started under.
    const RP: &str = "tak.example.com";

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn a_ceremony_is_good_for_exactly_one_attempt() {
        let db = database().await;
        let handle = begin(&db, CeremonyKind::Discover, RP, b"state")
            .await
            .unwrap();

        let claimed = claim(&db, &handle).await.unwrap();
        assert!(matches!(claimed.kind, CeremonyKind::Discover));
        assert_eq!(state_of(&claimed).unwrap(), b"state");
        assert!(require_rp_id(&claimed, RP).is_ok());

        assert!(
            claim(&db, &handle).await.is_err(),
            "a handle that could be presented twice is a replay",
        );
    }

    #[tokio::test]
    async fn an_unknown_handle_and_an_expired_one_are_refused_the_same_way() {
        let db = database().await;

        let unknown = claim(&db, "not-a-handle").await.unwrap_err();

        let handle = begin(&db, CeremonyKind::Discover, RP, b"").await.unwrap();

        // Reach past the API to age it, which is the only thing a test can do
        // about a five-minute window.
        let key = format!("{CEREMONY_PREFIX}{handle}");
        let mut stored: Ceremony = db
            .get(CEREMONY_PARTITION, key.clone())
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        db.set(CEREMONY_PARTITION, key, stored).await.unwrap();

        let expired = claim(&db, &handle).await.unwrap_err();

        assert_eq!(unknown.description(), expired.description());
    }

    #[tokio::test]
    async fn sweeping_takes_away_the_ceremonies_nobody_finished() {
        let db = database().await;
        let live = begin(&db, CeremonyKind::Discover, RP, b"").await.unwrap();
        let stale = begin(&db, CeremonyKind::Discover, RP, b"").await.unwrap();

        let key = format!("{CEREMONY_PREFIX}{stale}");
        let mut stored: Ceremony = db
            .get(CEREMONY_PARTITION, key.clone())
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        db.set(CEREMONY_PARTITION, key, stored).await.unwrap();

        assert_eq!(sweep(&db).await.unwrap(), 1);
        assert!(claim(&db, &live).await.is_ok());
    }

    #[tokio::test]
    async fn a_sweep_is_not_stopped_by_a_record_of_another_shape() {
        // R-01 M8. Four incompatible shapes shared one partition and `list`
        // deserialises every row into one type, so a single `setup-token` row —
        // written on the first start of every installation — aborted this sweep
        // from first boot until the wizard finished. Each family now has its
        // own partition; a stray row in another one must not be seen here.
        let db = database().await;
        let stale = begin(&db, CeremonyKind::Discover, RP, b"").await.unwrap();

        db.set(
            crate::auth::setup::AUTH_STATE_PARTITION,
            "setup-token".to_string(),
            serde_json::json!({ "hash": "x", "created_at": Utc::now() }),
        )
        .await
        .unwrap();

        let key = format!("{CEREMONY_PREFIX}{stale}");
        let mut stored: Ceremony = db
            .get(CEREMONY_PARTITION, key.clone())
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        db.set(CEREMONY_PARTITION, key, stored).await.unwrap();

        assert_eq!(sweep(&db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_ceremony_cannot_be_finished_under_another_relying_party() {
        // R-01 M10. The verifier is rebuilt per request, so without the pin a
        // ceremony started under one host name could be completed under
        // another.
        let db = database().await;
        let handle = begin(&db, CeremonyKind::Discover, RP, b"").await.unwrap();

        let claimed = claim(&db, &handle).await.unwrap();

        assert!(require_rp_id(&claimed, "evil.example.net").is_err());
        assert!(
            require_rp_id(
                &Ceremony {
                    kind: CeremonyKind::Discover,
                    state: String::new(),
                    rp_id: String::new(),
                    expires_at: Utc::now() + Duration::minutes(1),
                },
                RP,
            )
            .is_err(),
            "a record written before the pin existed is not a flow to complete",
        );
    }

    #[tokio::test]
    async fn a_ceremony_can_be_claimed_by_exactly_one_of_two_racing_callers() {
        // R-01 M13. `get` reads from a pool and `remove` goes through the
        // single writer, so two parallel posts of the same handle both used to
        // see the record and both proceed.
        let db = database().await;
        let handle = begin(&db, CeremonyKind::Discover, RP, b"").await.unwrap();

        let (first, second) = tokio::join!(claim(&db, &handle), claim(&db, &handle));

        assert!(
            first.is_ok() != second.is_ok(),
            "a challenge must be claimable once",
        );
    }

    #[test]
    fn an_accounts_handle_is_the_same_every_time_it_is_asked_for() {
        assert_eq!(user_handle(UserId::new(7)), user_handle(UserId::new(7)));
        assert_ne!(user_handle(UserId::new(7)), user_handle(UserId::new(8)));
        assert_eq!(user_handle(UserId::new(7)).as_ref().len(), USER_HANDLE_LEN);
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

    #[test]
    fn a_rows_transports_survive_the_round_trip_and_a_name_we_do_not_know_is_dropped() {
        // The list is a hint the browser uses to suggest how to reach the
        // authenticator. A transport invented after this version shipped must
        // not be able to stop somebody signing in.
        let row = row_with(Some(vec![
            "internal".to_string(),
            "hybrid".to_string(),
            "teleport".to_string(),
        ]));

        let transports = transports_of(&row);

        assert!(transports.contains(AuthenticatorTransport::Internal));
        assert!(transports.contains(AuthenticatorTransport::Hybrid));
        assert!(!transports.contains(AuthenticatorTransport::Usb));
        assert_eq!(
            transport_names(transports),
            Some(vec!["hybrid".to_string(), "internal".to_string()]),
        );
        assert_eq!(transport_names(AuthTransports::NONE), None);
    }

    #[test]
    fn the_backup_flags_a_row_carries_are_the_ones_the_check_is_given() {
        for (eligible, state, expected) in [
            (false, false, Backup::NotEligible),
            (true, false, Backup::Eligible),
            (true, true, Backup::Exists),
        ] {
            let mut row = row_with(None);
            row.backup_eligible = eligible;
            row.backup_state = state;

            let dynamic = dynamic_state_of(&row);

            assert_eq!(dynamic.backup, expected);
            assert!(dynamic.user_verified);
            assert_eq!(dynamic.sign_count, row.sign_count);
        }
    }

    #[test]
    fn a_public_key_written_by_something_else_is_a_system_error_rather_than_a_panic() {
        let mut row = row_with(None);
        row.public_key = vec![0xff; 4];

        let refused = static_state_of(&row).unwrap_err();

        assert!(refused.is(human_errors::Kind::System));
    }

    fn row_with(transports: Option<Vec<String>>) -> PasskeyRow {
        PasskeyRow {
            id: rustak_api::identity::PasskeyId::new(1),
            user_id: UserId::new(1),
            credential_id: vec![7; 16],
            public_key: Vec::new(),
            sign_count: 3,
            transports,
            label: "Phone".to_string(),
            backup_eligible: false,
            backup_state: false,
            created_at: Utc::now(),
            last_used_at: None,
        }
    }
}
