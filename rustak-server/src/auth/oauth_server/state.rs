//! The half-finished sign-in that lives between `/login/auth` and
//! `/login/redirect`.
//!
//! # Why the state is on the server as well as in a cookie
//!
//! TAK Server's rule is a cookie holding a random `state` and a `state` query
//! parameter holding its SHA-256; the callback is accepted when the two agree.
//! That is a genuine cross-site-request-forgery control — nobody can make
//! another person's browser present a callback matching a cookie they cannot
//! read — and it is reproduced here exactly, because a client written against
//! TAK Server will send what TAK Server taught it to send.
//!
//! It is not, on its own, enough. A cookie comparison says *this browser
//! started a flow*; it does not say *which* flow, it cannot expire, and it
//! cannot be spent. So the state also keys a server-side [`PendingAuth`]:
//! one-shot, ten minutes, and carrying the two secrets the callback needs and
//! the browser must never see — the proof-key verifier for the provider's code
//! and the nonce the provider's ID token has to echo.
//!
//! Claiming is the read *and* the delete, in that order, so a callback replayed
//! straight back at us finds nothing even in the window before a sweep would
//! have taken it away.
//!
//! # Where it is kept
//!
//! The `auth-state` key/value partition, beside the passkey ceremonies: it is
//! small, opaque, owned by exactly one component and read back whole, which is
//! design 01 §4.4's test for what belongs in the key/value store rather than in
//! a table of its own.

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

use crate::auth::setup::AUTH_STATE_PARTITION;
use crate::db::Database;
use crate::prelude::*;

/// The cookie the browser carries between the two halves of the flow.
pub const STATE_COOKIE: &str = "state";

/// How long a half-finished sign-in may stand.
///
/// Long enough for somebody to read a consent screen and type a one-time code
/// into their provider, short enough that an abandoned flow is not a credential
/// lying around.
pub const PENDING_TTL_MINUTES: i64 = 10;

/// How many random bytes the `state` carries.
const STATE_BYTES: usize = 32;

/// The prefix a pending sign-in is keyed by inside `auth-state`.
const PENDING_PREFIX: &str = "idp:";

/// Base64 as the flow uses it: URL-safe, unpadded, so the value survives a
/// query string and a cookie unescaped.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// What the browser is in the middle of.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PendingKind {
    /// Somebody signing in to this server directly. `return_to` is a path on
    /// this site, never an absolute URI: an open redirect on the end of a
    /// sign-in is a phishing primitive.
    Browser {
        /// Where to send them afterwards.
        return_to: Option<String>,
    },

    /// An OAuth2 client waiting for a code from `GET /oauth/authorize`.
    AuthorizationCode {
        /// The registered client that asked.
        client_id: String,
        /// The registered URI the code will be delivered to.
        redirect_uri: String,
        /// The client's own `state`, returned untouched.
        client_state: Option<String>,
        /// The client's `S256` proof-key challenge.
        code_challenge: String,
    },
}

/// A sign-in that has been started and not finished.
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingAuth {
    /// What the browser is in the middle of.
    pub kind: PendingKind,

    /// **Our** proof-key verifier, for redeeming the provider's code.
    pub verifier: String,

    /// The value the provider's ID token has to echo.
    pub nonce: String,

    /// The `redirect_uri` we sent the provider, which the token exchange has to
    /// repeat exactly.
    pub redirect_uri: String,

    /// When this stops being claimable.
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for PendingAuth {
    /// Written out because two fields are secrets: a `{:?}` in a log line would
    /// hand whoever reads it the ability to finish somebody else's sign-in.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuth")
            .field("kind", &self.kind)
            .field("verifier", &"***")
            .field("nonce", &"***")
            .field("redirect_uri", &self.redirect_uri)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A fresh `state`, for the cookie.
pub fn new_state() -> String {
    let mut bytes = [0u8; STATE_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    B64.encode(bytes)
}

/// A fresh nonce, for the provider's ID token to echo.
pub fn new_nonce() -> String {
    new_state()
}

/// The value sent to the provider, and the key the pending sign-in is stored
/// under: the SHA-256 of the cookie, base64url.
pub fn state_hash(state: &str) -> String {
    B64.encode(Sha256::digest(state.as_bytes()))
}

/// Whether a callback's `state` belongs to the `state` cookie this browser sent.
///
/// TAK Server's rule, compared without leaking where two values first differ.
pub fn matches_state(cookie: &str, returned: &str) -> bool {
    !cookie.is_empty()
        && !returned.is_empty()
        && super::constant_time_eq(&state_hash(cookie), returned)
}

/// Records a sign-in that has been started.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the record cannot be written.
pub async fn begin(db: &Database, state: &str, pending: PendingAuth) -> Result<(), Error> {
    db.set(AUTH_STATE_PARTITION, key(&state_hash(state)), pending)
        .await
}

/// Spends a pending sign-in, whether or not it had expired.
///
/// One shot: the record is removed before its expiry is looked at, so a
/// callback cannot be presented twice even inside the window a sweep would not
/// yet have covered.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when there is no such record or it has
/// expired — the two are not distinguished — and a
/// [`human_errors::Kind::System`] error when a read or write fails.
pub async fn claim(db: &Database, state_hash: &str) -> Result<PendingAuth, Error> {
    let key = key(state_hash);

    let Some(pending) = db
        .get::<PendingAuth>(AUTH_STATE_PARTITION, key.clone())
        .await?
    else {
        return Err(expired());
    };

    db.remove(AUTH_STATE_PARTITION, key).await?;

    if pending.expires_at <= Utc::now() {
        return Err(expired());
    }

    Ok(pending)
}

/// When a sign-in started now stops being claimable.
pub fn expiry() -> DateTime<Utc> {
    Utc::now() + Duration::minutes(PENDING_TTL_MINUTES)
}

/// Removes every pending sign-in that has expired.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or write fails.
pub async fn sweep(db: &Database) -> Result<usize, Error> {
    let now = Utc::now();
    let stored: Vec<(String, PendingAuth)> = db.list(AUTH_STATE_PARTITION).await?;
    let mut removed = 0;

    for (key, pending) in stored {
        if key.starts_with(PENDING_PREFIX) && pending.expires_at <= now {
            db.remove(AUTH_STATE_PARTITION, key).await?;
            removed += 1;
        }
    }

    Ok(removed)
}

/// Where one pending sign-in is stored.
fn key(state_hash: &str) -> String {
    format!("{PENDING_PREFIX}{state_hash}")
}

/// The one thing a callback we cannot place is ever told.
fn expired() -> Error {
    human_errors::user(
        "That sign-in could not be completed.",
        &["Start signing in again from the beginning."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> PendingAuth {
        PendingAuth {
            kind: PendingKind::Browser { return_to: None },
            verifier: "a-verifier".to_string(),
            nonce: "a-nonce".to_string(),
            redirect_uri: "https://tak.example.com/login/redirect".to_string(),
            expires_at: expiry(),
        }
    }

    #[test]
    fn the_value_sent_to_the_provider_is_the_digest_of_the_cookie() {
        // TAK Server's rule, which a client written against it will send.
        let state = new_state();

        assert!(matches_state(&state, &state_hash(&state)));
        assert_ne!(state, state_hash(&state));
    }

    #[test]
    fn a_state_that_did_not_come_from_this_cookie_is_refused() {
        let state = new_state();

        for returned in [
            state.clone(),
            state_hash("another-flows-state"),
            String::new(),
            format!("{}x", state_hash(&state)),
        ] {
            assert!(!matches_state(&state, &returned), "{returned}");
        }

        assert!(
            !matches_state("", &state_hash("")),
            "a browser with no cookie must not be able to satisfy the check by sending none",
        );
    }

    #[test]
    fn two_flows_never_share_a_state() {
        assert_ne!(new_state(), new_state());
        assert_ne!(new_nonce(), new_nonce());
    }

    #[tokio::test]
    async fn a_pending_sign_in_is_claimable_exactly_once() {
        let db = Database::open_in_memory().await.unwrap();
        let state = new_state();

        begin(&db, &state, pending()).await.unwrap();

        let claimed = claim(&db, &state_hash(&state)).await.unwrap();
        assert_eq!(claimed.nonce, "a-nonce");

        assert!(
            claim(&db, &state_hash(&state)).await.is_err(),
            "a callback replayed straight back at us has to find nothing",
        );
    }

    #[tokio::test]
    async fn an_expired_pending_sign_in_is_refused_and_removed() {
        let db = Database::open_in_memory().await.unwrap();
        let state = new_state();

        begin(
            &db,
            &state,
            PendingAuth {
                expires_at: Utc::now() - Duration::seconds(1),
                ..pending()
            },
        )
        .await
        .unwrap();

        assert!(claim(&db, &state_hash(&state)).await.is_err());
    }

    #[tokio::test]
    async fn a_state_nobody_started_is_refused_the_same_way_an_expired_one_is() {
        let db = Database::open_in_memory().await.unwrap();

        let refused = claim(&db, &state_hash("never-started")).await.unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(!refused.description().contains("expired"));
    }

    #[tokio::test]
    async fn sweeping_removes_the_expired_and_leaves_the_live() {
        let db = Database::open_in_memory().await.unwrap();
        let live = new_state();
        let stale = new_state();

        begin(&db, &live, pending()).await.unwrap();
        begin(
            &db,
            &stale,
            PendingAuth {
                expires_at: Utc::now() - Duration::minutes(1),
                ..pending()
            },
        )
        .await
        .unwrap();

        assert_eq!(sweep(&db).await.unwrap(), 1);
        assert!(claim(&db, &state_hash(&live)).await.is_ok());
    }

    #[test]
    fn the_secrets_are_redacted_when_a_pending_sign_in_is_printed() {
        let rendered = format!("{:?}", pending());

        assert!(!rendered.contains("a-verifier"), "{rendered}");
        assert!(!rendered.contains("a-nonce"), "{rendered}");
        assert!(rendered.contains("***"), "{rendered}");
    }
}
