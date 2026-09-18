//! Signing in, and the passkey ceremonies.
//!
//! The token endpoints themselves (`/auth/token` and `/auth/refresh`) are not
//! here: they live in [`crate::auth`] and go straight to `gloo_net`, because
//! routing a token renewal through a client that renews tokens on 401 is how one
//! gets an infinite loop.

use rustak_api::{
    AuthMetadata, Me, PasskeyChallenge, PasskeyLoginFinish, PasskeyLoginStart,
    PasskeyRegistrationFinish, PasskeyRegistrationStart, PasskeySummary, TokenResponse, Username,
};

use crate::api::{ApiError, Verb, error_from_response, get_json, json_response, post_json, send};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// What the login page needs before it can draw itself: which sign-in methods
/// this installation accepts.
pub async fn metadata() -> Result<AuthMetadata, ApiError> {
    demo!(Ok(fixtures::auth_metadata()));

    get_json("/auth/metadata").await
}

/// Who the caller is.
pub async fn me() -> Result<Me, ApiError> {
    demo!(fixtures::me().ok_or(ApiError::Unauthorized));

    get_json("/me").await
}

/// Drops the session on the server as well as in this tab.
///
/// A failure is not reported: the local session is cleared either way, and there
/// is nothing useful for somebody who has just signed out to do about a server
/// that did not hear them.
pub async fn logout() {
    demo!(fixtures::sign_out());

    let _ = send::<()>(Verb::Post, "/auth/logout", None).await;
}

/// Begins registering a passkey, for somebody who is signed in or who holds the
/// wizard's short-lived registration token.
pub async fn passkey_register_start(
    request: &PasskeyRegistrationStart,
) -> Result<PasskeyChallenge, ApiError> {
    demo!(Ok(fixtures::passkey_challenge()));

    post_json("/auth/passkey/register/start", request).await
}

/// Completes a registration.
///
/// The response is read loosely on purpose. A server that hands back a session
/// along with the new passkey saves the wizard a second ceremony, and one that
/// answers with the passkey alone (or with nothing) is equally correct — so both
/// are accepted and the caller signs in explicitly when no token arrives.
pub async fn passkey_register_finish(
    request: &PasskeyRegistrationFinish,
) -> Result<Option<TokenResponse>, ApiError> {
    demo!(Ok(Some(fixtures::register_passkey(
        request.label.as_deref()
    ))));

    let response = send(Verb::Post, "/auth/passkey/register/finish", Some(request)).await?;
    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    Ok(response.json::<TokenResponse>().await.ok())
}

/// Begins a passkey sign-in. Omitting the username asks the authenticator which
/// account it holds, which is the better flow: naming an account before proving
/// anything is what lets somebody ask the server which accounts exist.
pub async fn passkey_login_start(username: Option<Username>) -> Result<PasskeyChallenge, ApiError> {
    demo!(Ok(fixtures::passkey_challenge()));

    post_json("/auth/passkey/login/start", &PasskeyLoginStart { username }).await
}

/// Completes a sign-in, returning the session it established.
pub async fn passkey_login_finish(request: &PasskeyLoginFinish) -> Result<TokenResponse, ApiError> {
    demo!(Ok(fixtures::sign_in_with_passkey()));

    post_json("/auth/passkey/login/finish", request).await
}

/// The passkeys registered against the signed-in account.
pub async fn list_passkeys() -> Result<Vec<PasskeySummary>, ApiError> {
    demo!(Ok(fixtures::passkeys()));

    get_json("/auth/passkeys").await
}

/// Removes a passkey.
pub async fn delete_passkey(id: i64) -> Result<(), ApiError> {
    demo!(fixtures::delete_passkey(id); Ok(()));

    let response = send::<()>(Verb::Delete, &format!("/auth/passkeys/{id}"), None).await?;
    if response.ok() {
        Ok(())
    } else {
        Err(error_from_response(response).await)
    }
}

/// Exchanges an identity provider's authorization code for our own session.
///
/// Used by the callback page. It goes through the bearer-aware client because
/// there is no session yet for a 401 to renew, so the retry cannot loop.
pub async fn exchange_code(
    body: &rustak_api::TokenExchangeRequest,
) -> Result<TokenResponse, ApiError> {
    json_response(send(Verb::Post, "/auth/token", Some(body)).await?).await
}
