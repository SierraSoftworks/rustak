//! Sessions: the access token a caller presents, and the refresh token that
//! renews it.
//!
//! Whatever somebody signed in with — an identity provider, a passkey, the
//! first-run wizard — the session that follows is the same pair. One token
//! format for the admin UI, CloudTAK and sidecars is what lets rustak be the
//! identity authority for all of them, and it is pinned in [`jwt`].
//!
//! # Rotation, and what reuse means
//!
//! A refresh token is spent when it is exchanged and a new one issued in the
//! same family. Presenting a spent one therefore means one of two things: the
//! client lost the response and retried, or somebody else has a copy. We cannot
//! tell which, so the whole family is revoked — the legitimate client signs in
//! again, and a stolen token buys nothing. That is the standard treatment, and
//! it is the reason refresh tokens rotate at all.
//!
//! # Why the refresh token is opaque
//!
//! It is stored as a SHA-256, never in full, and carries no claims. It is high
//! entropy and only ever compared, so there is nothing for argon2 to defend
//! against and nothing for a reader of the database to learn.
//!
//! [`jwt`]: super::jwt

use base64::Engine as _;
use chrono::Utc;
use rand::Rng as _;
use rustak_api::TokenResponse;
use sha2::{Digest as _, Sha256};

use crate::db::repos::{Exchange, NewRefreshToken, UserRow};
use crate::prelude::*;

/// The scope every session carries.
pub const SCOPE_API: &str = "api";

/// The scope added for somebody who may administer the installation.
pub const SCOPE_ADMIN: &str = "admin";

/// How many random bytes a refresh token carries.
const REFRESH_BYTES: usize = 32;

/// Base64 as the tokens use it: URL-safe, unpadded, so it survives a JSON body
/// and a header without escaping.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The scope string for a principal.
pub fn scope_for(is_admin: bool) -> String {
    if is_admin {
        format!("{SCOPE_API} {SCOPE_ADMIN}")
    } else {
        SCOPE_API.to_string()
    }
}

/// Issues a fresh session: a signed access token and a new refresh family.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the token cannot be signed or
/// the refresh token cannot be stored.
#[instrument("auth.tokens.issue", skip_all, fields(username = %user.username), err(Display))]
pub async fn issue_session<S: Services>(
    services: &S,
    user: &UserRow,
    is_admin: bool,
    client: Option<&str>,
) -> Result<TokenResponse, Error> {
    let config = services.config();
    let jwt = services.jwt()?;
    let scope = scope_for(is_admin);

    let (token, claims) = jwt.issue(&user.username, &scope, None, None)?;
    let refresh = mint(
        services,
        user,
        &scope,
        client,
        uuid::Uuid::new_v4().to_string(),
    )
    .await?;

    Ok(TokenResponse::new(
        token,
        refresh,
        expires_in(&config, claims.exp),
    ))
}

/// Exchanges a refresh token for a new pair.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error whenever the token is not one we would
/// accept — unknown, expired, already spent, or belonging to an account that
/// has since been disabled — all reported the same way, so the endpoint is not
/// an oracle for which. A [`human_errors::Kind::System`] error if a read or a
/// write fails.
#[instrument("auth.tokens.rotate", skip_all, err(Display))]
pub async fn rotate<S: Services>(
    services: &S,
    refresh_token: &str,
    client: Option<&str>,
) -> Result<TokenResponse, Error> {
    let config = services.config();
    let db = services.db();

    let spent = match db.refresh_tokens().exchange(&hash(refresh_token)).await? {
        Exchange::Spent(row) => *row,
        Exchange::Replayed { family, revoked } => {
            warn!(
                family = %family,
                revoked,
                "A refresh token was presented twice; its whole family has been revoked."
            );

            return Err(rejected());
        }
        Exchange::Unknown => return Err(rejected()),
    };

    let Some(user) = db.users().get(spent.user_id).await? else {
        return Err(rejected());
    };

    if user.disabled {
        db.refresh_tokens().revoke_family(&spent.family).await?;

        return Err(rejected());
    }

    let is_admin = user.is_effective_admin();
    let scope = scope_for(is_admin);
    let (token, claims) = services.jwt()?.issue(&user.username, &scope, None, None)?;
    let refresh = mint(services, &user, &scope, client, spent.family).await?;

    Ok(TokenResponse::new(
        token,
        refresh,
        expires_in(&config, claims.exp),
    ))
}

/// Ends a session: the access token is revoked by its `jti`, and the refresh
/// family with it.
///
/// A failure to revoke the family is logged rather than returned, because the
/// access token has already been listed by then and signing somebody out must
/// not fail halfway.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the revocation cannot be recorded.
#[instrument("auth.tokens.revoke", skip_all, err(Display))]
pub async fn revoke<S: Services>(
    services: &S,
    jti: &str,
    expires_at: chrono::DateTime<Utc>,
    user_id: UserId,
) -> Result<(), Error> {
    let db = services.db();

    db.revoked_jtis().revoke(jti, expires_at).await?;

    if let Err(err) = db.refresh_tokens().revoke_all_for_user(user_id).await {
        warn!(error = %err, "Could not revoke the refresh tokens of somebody signing out.");
        services.session().record_human_error(&err);
    }

    Ok(())
}

/// Stores a fresh refresh token and returns the half the caller keeps.
async fn mint<S: Services>(
    services: &S,
    user: &UserRow,
    scope: &str,
    client: Option<&str>,
    family: String,
) -> Result<String, Error> {
    let config = services.config();
    let mut bytes = [0u8; REFRESH_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    let token = B64.encode(bytes);

    services
        .db()
        .refresh_tokens()
        .create(NewRefreshToken {
            user_id: user.id,
            token_hash: hash(&token),
            family,
            scope: scope.to_string(),
            client: client.map(str::to_string),
            expires_at: Utc::now() + config.auth.refresh_token_ttl,
        })
        .await?;

    Ok(token)
}

/// How long the access token has left, in whole seconds.
fn expires_in(config: &crate::config::Config, exp: i64) -> u64 {
    let remaining = exp - Utc::now().timestamp();

    u64::try_from(remaining).unwrap_or(config.auth.access_token_ttl.num_seconds().max(0) as u64)
}

/// The stored form of a refresh token.
///
/// Plain SHA-256 rather than argon2: the token is 256 bits of randomness we
/// generated, so there is no dictionary for a slow hash to defend against and a
/// per-request key stretch would only cost latency.
fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The one thing a refused refresh is ever told.
fn rejected() -> Error {
    human_errors::user(
        "That session could not be renewed.",
        &["Sign in again to start a new session."],
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::db::repos::NewUser;

    async fn context() -> AppContext {
        let context = AppContext::new_mock(|config| {
            config.server.domains = vec!["tak.example.com".to_string()];
        })
        .await
        .unwrap();

        let issuer = crate::auth::JwtIssuer::load_or_create(
            context.db(),
            context.secrets(),
            &context.config().auth,
            "https://tak.example.com",
        )
        .await
        .unwrap();

        context.install_jwt(Arc::new(issuer)).unwrap();

        context
    }

    async fn user(context: &AppContext, admin: bool) -> UserRow {
        context
            .db()
            .users()
            .create(NewUser {
                is_admin: admin,
                ..NewUser::person(Username::parse("ada").unwrap())
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_session_is_a_signed_token_and_a_refresh_token() {
        let context = context().await;
        let user = user(&context, false).await;

        let session = issue_session(&context, &user, false, Some("ui"))
            .await
            .unwrap();

        assert_eq!(session.token_type, "Bearer");
        assert!(session.expires_in > 0);

        let claims = context.jwt().unwrap().verify(&session.token).unwrap();
        assert_eq!(claims.sub, "ada");
        assert_eq!(claims.scope, SCOPE_API);
    }

    #[tokio::test]
    async fn an_administrators_token_says_so_in_its_scope() {
        let context = context().await;
        let user = user(&context, true).await;

        let session = issue_session(&context, &user, true, None).await.unwrap();
        let claims = context.jwt().unwrap().verify(&session.token).unwrap();

        assert!(claims.scope.split(' ').any(|scope| scope == SCOPE_ADMIN));
    }

    #[tokio::test]
    async fn renewing_spends_the_old_token_and_issues_a_new_one() {
        let context = context().await;
        let user = user(&context, false).await;

        let first = issue_session(&context, &user, false, None).await.unwrap();
        let refresh = first.refresh_token.clone().unwrap();

        let second = rotate(&context, &refresh, None).await.unwrap();

        assert_ne!(second.refresh_token, first.refresh_token);
        assert!(context.jwt().unwrap().verify(&second.token).is_ok());
    }

    #[tokio::test]
    async fn presenting_a_spent_token_ends_the_whole_family() {
        // We cannot tell a retry from a theft, so the only safe reading is the
        // second one: everything descended from that token stops working.
        let context = context().await;
        let user = user(&context, false).await;

        let first = issue_session(&context, &user, false, None).await.unwrap();
        let refresh = first.refresh_token.clone().unwrap();
        let second = rotate(&context, &refresh, None).await.unwrap();

        assert!(rotate(&context, &refresh, None).await.is_err());
        assert!(
            rotate(&context, &second.refresh_token.clone().unwrap(), None)
                .await
                .is_err(),
            "the token the legitimate client holds is revoked with the rest of its family",
        );
    }

    #[tokio::test]
    async fn an_unknown_token_is_refused_the_same_way_a_spent_one_is() {
        let context = context().await;

        let refused = rotate(&context, "not-a-token", None).await.unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(!refused.description().contains("unknown"));
    }

    #[tokio::test]
    async fn a_disabled_account_cannot_renew() {
        let context = context().await;
        let user = user(&context, false).await;
        let session = issue_session(&context, &user, false, None).await.unwrap();

        context
            .db()
            .users()
            .set_disabled(user.id, true)
            .await
            .unwrap();

        assert!(
            rotate(&context, &session.refresh_token.unwrap(), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_renewal_picks_up_an_account_that_has_become_an_administrator() {
        // The alternative is somebody having to sign out and back in before a
        // change an administrator just made takes effect.
        let context = context().await;
        let user = user(&context, false).await;
        let session = issue_session(&context, &user, false, None).await.unwrap();

        context
            .db()
            .users()
            .set_admin_override(user.id, Some(true))
            .await
            .unwrap();

        let renewed = rotate(&context, &session.refresh_token.unwrap(), None)
            .await
            .unwrap();
        let claims = context.jwt().unwrap().verify(&renewed.token).unwrap();

        assert!(claims.scope.split(' ').any(|scope| scope == SCOPE_ADMIN));
    }

    #[tokio::test]
    async fn signing_out_revokes_the_token_and_every_refresh_token_behind_it() {
        let context = context().await;
        let user = user(&context, false).await;
        let session = issue_session(&context, &user, false, None).await.unwrap();
        let claims = context.jwt().unwrap().verify(&session.token).unwrap();

        revoke(
            &context,
            &claims.jti,
            chrono::DateTime::from_timestamp(claims.exp, 0).unwrap(),
            user.id,
        )
        .await
        .unwrap();

        assert!(
            context
                .db()
                .revoked_jtis()
                .is_revoked(&claims.jti)
                .await
                .unwrap()
        );
        assert!(
            rotate(&context, &session.refresh_token.unwrap(), None)
                .await
                .is_err()
        );
    }

    #[test]
    fn a_stored_refresh_token_is_a_digest_rather_than_the_token() {
        let digest = hash("the-token");

        assert_eq!(digest.len(), 64);
        assert!(!digest.contains("the-token"));
        assert_eq!(digest, hash("the-token"));
    }
}
