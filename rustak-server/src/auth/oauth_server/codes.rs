//! The authorization codes **we** issue, and the bindings they carry.
//!
//! A code is 32 random bytes handed to a browser through a redirect. It is
//! therefore visible in an address bar, in a `Referer`, in a proxy log and in
//! browser history, and the whole design of this module starts from assuming
//! somebody else has a copy.
//!
//! Four things have to hold before one is exchanged for a session, and all four
//! are checked inside a single transaction so that two simultaneous redemptions
//! cannot both win:
//!
//! 1. It was issued to the client now redeeming it.
//! 2. It was issued for exactly this `redirect_uri` — byte for byte, never a
//!    prefix.
//! 3. The caller holds the proof-key verifier whose `S256` hash was registered
//!    when the code was issued. This is the one that makes a stolen code
//!    worthless, and it is why `plain` is not accepted anywhere.
//! 4. It has not been redeemed already, and it has not expired.
//!
//! A failure of (1), (2) or (3) deliberately does **not** consume the code: a
//! wrong guess must not be a way to burn the code the legitimate browser is
//! about to redeem. Guessing is not a concern — the verifier is 256 bits.
//!
//! The code itself is never stored, only its SHA-256: it is randomness we
//! generated, so there is no dictionary for a slow hash to defend against and
//! nothing for a reader of the database to learn.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use rand::Rng as _;
use rusqlite::OptionalExtension as _;
use sha2::{Digest as _, Sha256};

use crate::db::{Database, row::Timestamp};
use crate::prelude::*;
use crate::web::helpers::oidc::pkce;

use super::constant_time_eq;

/// How long a code may stand unredeemed.
///
/// RFC 6749 §4.1.2 recommends a maximum of ten minutes, and a browser redirect
/// takes milliseconds — the window exists for a slow network and a user tabbing
/// away, not for anything a client legitimately waits on.
pub const CODE_TTL_MINUTES: i64 = 10;

/// How many random bytes a code carries.
const CODE_BYTES: usize = 32;

/// The only challenge method this server will register.
pub const S256: &str = "S256";

/// Base64 as OAuth uses it: URL-safe, unpadded, so it survives a query string.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A code about to be issued.
#[derive(Debug, Clone)]
pub struct NewCode {
    /// The registered client it is issued to.
    pub client_id: String,
    /// Whose session it will become.
    pub user_id: UserId,
    /// The exact URI it will be delivered to.
    pub redirect_uri: String,
    /// The scope of the session it will be exchanged for.
    pub scope: String,
    /// The client's `S256` proof-key challenge.
    pub code_challenge: String,
}

/// What a redemption established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redemption {
    /// Whose session this becomes.
    pub user_id: UserId,
    /// The scope recorded when the code was issued.
    pub scope: String,
}

/// Why a code was not redeemed.
#[derive(Debug)]
pub enum CodeError {
    /// Unknown, expired, already spent, or not matching one of its bindings.
    ///
    /// One variant for all of them on purpose: telling a caller *which* check
    /// failed turns this endpoint into an oracle for which codes exist and
    /// which redirect URIs are registered.
    Invalid,

    /// Something of ours failed.
    Unavailable(Error),
}

impl From<Error> for CodeError {
    fn from(err: Error) -> Self {
        Self::Unavailable(err)
    }
}

/// Issues a code, returning the half the browser carries.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the row cannot be written.
#[instrument("auth.oauth.codes.issue", skip_all, fields(client = %new.client_id), err(Display))]
pub async fn issue(db: &Database, new: NewCode) -> Result<String, Error> {
    let mut bytes = [0u8; CODE_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    let code = B64.encode(bytes);
    let expires_at = Utc::now() + chrono::Duration::minutes(CODE_TTL_MINUTES);
    let hash = hash(&code);

    db.write(move |tx| {
        tx.execute(
            "INSERT INTO oauth_codes \
               (code_hash, client_id, user_id, redirect_uri, scope, code_challenge, \
                code_challenge_method, created_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                hash,
                new.client_id,
                new.user_id.get(),
                new.redirect_uri,
                new.scope,
                new.code_challenge,
                S256,
                Timestamp::now(),
                Timestamp::from(expires_at),
            ],
        )
    })
    .await?;

    Ok(code)
}

/// Spends a code, once, if every binding it carries still holds.
///
/// # Errors
///
/// [`CodeError::Invalid`] for anything the caller did, and
/// [`CodeError::Unavailable`] when the write fails.
#[instrument("auth.oauth.codes.redeem", skip_all, fields(client = %client_id), err(Debug))]
pub async fn redeem(
    db: &Database,
    code: &str,
    client_id: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<Redemption, CodeError> {
    let hash = hash(code);
    let client_id = client_id.to_string();
    let redirect_uri = redirect_uri.to_string();
    let presented = pkce::challenge_for(code_verifier);

    db.write(move |tx| {
        let Some((stored_client, stored_uri, stored_challenge, user_id, scope, expires, consumed)) =
            tx.query_one(
                "SELECT client_id, redirect_uri, code_challenge, user_id, scope, expires_at, \
                        consumed_at \
                 FROM oauth_codes WHERE code_hash = ?1",
                [&hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        crate::db::row::id_col::<UserId>(row, 3)?,
                        row.get::<_, String>(4)?,
                        crate::db::row::ts(row, 5)?,
                        crate::db::row::opt_ts(row, 6)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };

        let now = Utc::now();
        let spent: Option<DateTime<Utc>> = consumed;

        if spent.is_some() || expires <= now {
            return Ok(None);
        }

        // Checked before the row is marked, so that a wrong guess cannot burn
        // the code the legitimate browser is about to present.
        if !constant_time_eq(&stored_client, &client_id)
            || !constant_time_eq(&stored_uri, &redirect_uri)
            || !constant_time_eq(&stored_challenge, &presented)
        {
            return Ok(None);
        }

        // `consumed_at IS NULL` in the predicate, not just in the read above:
        // it is what makes two simultaneous redemptions resolve to one winner
        // even though both read the row as live.
        let marked = tx.execute(
            "UPDATE oauth_codes SET consumed_at = ?2 \
             WHERE code_hash = ?1 AND consumed_at IS NULL",
            rusqlite::params![&hash, Timestamp::from(now)],
        )?;

        if marked == 0 {
            return Ok(None);
        }

        Ok(Some(Redemption { user_id, scope }))
    })
    .await?
    .ok_or(CodeError::Invalid)
}

/// Deletes codes that expired before `before`.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write fails.
pub async fn prune(db: &Database, before: DateTime<Utc>) -> Result<usize, Error> {
    let before = Timestamp::from(before);

    db.write(move |tx| tx.execute("DELETE FROM oauth_codes WHERE expires_at < ?1", [before]))
        .await
}

/// The stored form of a code. See the module documentation for why SHA-256.
fn hash(code: &str) -> String {
    hex::encode(Sha256::digest(code.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    /// A database with one account, and a code issued to `app`.
    async fn fixture() -> (Database, UserId, String, String) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("ada").unwrap()))
            .await
            .unwrap();
        let pkce = pkce::pkce_pair();

        let code = issue(
            &db,
            NewCode {
                client_id: "app".to_string(),
                user_id: user.id,
                redirect_uri: "https://app.example.com/cb".to_string(),
                scope: "api".to_string(),
                code_challenge: pkce.challenge.clone(),
            },
        )
        .await
        .unwrap();

        (db, user.id, code, pkce.verifier)
    }

    async fn redeemed(db: &Database, code: &str, verifier: &str) -> Result<Redemption, CodeError> {
        redeem(db, code, "app", "https://app.example.com/cb", verifier).await
    }

    #[tokio::test]
    async fn a_code_is_exchanged_once_and_never_again() {
        let (db, user, code, verifier) = fixture().await;

        let first = redeemed(&db, &code, &verifier).await.unwrap();

        assert_eq!(first.user_id, user);
        assert_eq!(first.scope, "api");
        assert!(
            matches!(
                redeemed(&db, &code, &verifier).await,
                Err(CodeError::Invalid)
            ),
            "a replayed code has to be worthless even to whoever redeemed it first",
        );
    }

    #[tokio::test]
    async fn a_code_without_its_verifier_is_worthless() {
        // The whole point of proof key for code exchange: a code lifted from a
        // redirect, an address bar or a proxy log buys nothing.
        let (db, _, code, verifier) = fixture().await;

        assert!(matches!(
            redeemed(&db, &code, "a-verifier-somebody-else-made-up").await,
            Err(CodeError::Invalid),
        ));
        assert!(matches!(
            redeemed(&db, &code, &format!("{verifier}x")).await,
            Err(CodeError::Invalid),
        ));

        assert!(
            redeemed(&db, &code, &verifier).await.is_ok(),
            "and a wrong verifier must not have burnt the code in the meantime",
        );
    }

    #[tokio::test]
    async fn a_code_is_bound_to_the_redirect_uri_it_was_issued_for() {
        let (db, _, code, verifier) = fixture().await;

        assert!(matches!(
            redeem(
                &db,
                &code,
                "app",
                "https://app.example.com/cb/elsewhere",
                &verifier
            )
            .await,
            Err(CodeError::Invalid),
        ));

        assert!(redeemed(&db, &code, &verifier).await.is_ok());
    }

    #[tokio::test]
    async fn a_code_is_bound_to_the_client_it_was_issued_to() {
        let (db, _, code, verifier) = fixture().await;

        assert!(matches!(
            redeem(
                &db,
                &code,
                "another-app",
                "https://app.example.com/cb",
                &verifier
            )
            .await,
            Err(CodeError::Invalid),
        ));
    }

    #[tokio::test]
    async fn an_expired_code_is_refused_and_pruned() {
        let (db, user, _, verifier) = fixture().await;
        let challenge = pkce::challenge_for(&verifier);
        let code = issue(
            &db,
            NewCode {
                client_id: "app".to_string(),
                user_id: user,
                redirect_uri: "https://app.example.com/cb".to_string(),
                scope: "api".to_string(),
                code_challenge: challenge,
            },
        )
        .await
        .unwrap();

        db.write(move |tx| {
            tx.execute(
                "UPDATE oauth_codes SET expires_at = ?1",
                [Timestamp::from(Utc::now() - chrono::Duration::minutes(1))],
            )
        })
        .await
        .unwrap();

        assert!(matches!(
            redeemed(&db, &code, &verifier).await,
            Err(CodeError::Invalid)
        ));
        assert_eq!(prune(&db, Utc::now()).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_code_nobody_issued_is_refused_the_same_way_a_spent_one_is() {
        let (db, _, _, verifier) = fixture().await;

        assert!(matches!(
            redeemed(&db, "not-a-code", &verifier).await,
            Err(CodeError::Invalid)
        ));
    }

    #[tokio::test]
    async fn deleting_an_account_takes_its_outstanding_codes_with_it() {
        let (db, user, code, verifier) = fixture().await;

        db.users().delete(user).await.unwrap();

        assert!(matches!(
            redeemed(&db, &code, &verifier).await,
            Err(CodeError::Invalid)
        ));
    }
}
