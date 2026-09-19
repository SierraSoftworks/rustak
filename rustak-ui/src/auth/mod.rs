//! The browser's half of signing in.
//!
//! Whichever way somebody signs in — through the identity provider
//! ([`oidc`]) or with a passkey ([`passkey`]) — the server issues **its own**
//! RS256 bearer token and a rotating refresh token, and this module is where
//! those live. Storing one format rather than two is what lets the API client
//! know nothing about which method produced the session it is carrying.
//!
//! # Where the tokens are kept
//!
//! In `sessionStorage`, under `rustak.admin.*`. That confines a session to the
//! tab that established it: closing the tab ends it, and another tab starts
//! signed out. `localStorage` is used for exactly one thing — the slot a
//! sign-in popup hands its result back through, because a popup does not share
//! `sessionStorage` with its opener — and that slot is cleared as soon as it is
//! read.

pub mod oidc;
pub mod passkey;
mod single_flight;

use base64::prelude::*;
use futures::FutureExt;
use rustak_api::TokenResponse;

// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::window;
use single_flight::SingleFlight;

/// sessionStorage key holding the bearer token.
pub const TOKEN_KEY: &str = "rustak.admin.token";
/// sessionStorage key holding the refresh token.
pub const REFRESH_KEY: &str = "rustak.admin.refresh";
/// sessionStorage key holding the in-flight OAuth `state` value.
pub const STATE_KEY: &str = "rustak.admin.oidc_state";
/// sessionStorage key holding the in-flight PKCE code verifier.
pub const VERIFIER_KEY: &str = "rustak.admin.oidc_verifier";
/// sessionStorage key saying what the in-flight popup was opened to do.
pub const INTENT_KEY: &str = "rustak.admin.oidc_intent";
/// localStorage slot the sign-in popup hands its result back through.
pub const POPUP_RESULT_KEY: &str = "rustak.admin.popup_result";

fn session() -> Option<web_sys::Storage> {
    window().session_storage().ok().flatten()
}

fn local() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

/// The stored bearer token, if this tab has a session.
pub fn stored_token() -> Option<String> {
    session()?.get_item(TOKEN_KEY).ok().flatten()
}

fn stored_refresh_token() -> Option<String> {
    session()?.get_item(REFRESH_KEY).ok().flatten()
}

/// Persists a session. The refresh token is only overwritten when a new one was
/// issued, so a server that does not rotate them does not lose the one we hold.
pub fn store_tokens(token: &str, refresh: Option<&str>) {
    if let Some(storage) = session() {
        let _ = storage.set_item(TOKEN_KEY, token);
        if let Some(refresh) = refresh {
            let _ = storage.set_item(REFRESH_KEY, refresh);
        }
    }
}

/// Stores the result of a successful sign-in of any kind.
pub fn store_session(tokens: &TokenResponse) {
    store_tokens(&tokens.token, tokens.refresh_token.as_deref());
}

/// Drops this tab's session, along with any half-finished sign-in.
pub fn clear_session() {
    if let Some(storage) = session() {
        let _ = storage.remove_item(TOKEN_KEY);
        let _ = storage.remove_item(REFRESH_KEY);
        let _ = storage.remove_item(STATE_KEY);
        let _ = storage.remove_item(VERIFIER_KEY);
        let _ = storage.remove_item(INTENT_KEY);
    }
}

/// Signs out here and, as a courtesy, on the server.
///
/// The local session goes first. Whether the server hears about it or not, the
/// tab that asked to sign out is signed out.
pub async fn sign_out() {
    clear_session();
    crate::api::auth::logout().await;
}

/// A high-entropy, URL-safe random string carrying `bytes` bytes of entropy.
///
/// Returns `None` when the platform cannot supply randomness, because the
/// alternative to an unpredictable value here is a predictable one, and every
/// caller would rather fail than use that.
pub fn random_token(bytes: usize) -> Option<String> {
    let mut buffer = vec![0u8; bytes];
    window()
        .crypto()
        .ok()?
        .get_random_values_with_u8_array(&mut buffer)
        .ok()?;
    Some(BASE64_URL_SAFE_NO_PAD.encode(&buffer))
}

thread_local! {
    /// Coalesces concurrent, 401-driven renewals onto a single redemption of the
    /// stored refresh token (see [`refresh_session`]).
    static REFRESH: SingleFlight<Result<String, String>> = SingleFlight::new();
}

/// Renews the session from the stored refresh token, returning a fresh bearer.
///
/// Concurrent callers are coalesced onto one renewal. Refresh tokens **rotate**:
/// redeeming one issues another and revokes the old, and reusing a spent one
/// revokes the whole family on the assumption that it was stolen. Several pages
/// polling at once can meet a 401 together, and were each to redeem the stored
/// token independently, the first would succeed and the rest would look like
/// theft. Sharing one redemption avoids that.
pub async fn refresh_session() -> Result<String, String> {
    REFRESH
        .with(|flight| flight.run(|| do_refresh().boxed_local()))
        .await
}

/// One renewal against the server.
///
/// It talks to `gloo_net` directly rather than through [`crate::api`], because
/// routing a renewal through a client that renews on 401 is how one gets a loop.
async fn do_refresh() -> Result<String, String> {
    demo!(Ok(fixtures::demo_token()));

    let refresh = stored_refresh_token().ok_or("no refresh token is available")?;
    let body = serde_json::json!({ "refresh_token": refresh });

    let response = gloo_net::http::Request::post("/api/v1/auth/refresh")
        .json(&body)
        .map_err(|err| err.to_string())?
        .send()
        .await
        .map_err(|err| err.to_string())?;

    if !response.ok() {
        clear_session();
        return Err(format!(
            "the session could not be renewed (HTTP {})",
            response.status()
        ));
    }

    let tokens = response
        .json::<TokenResponse>()
        .await
        .map_err(|err| err.to_string())?;
    store_session(&tokens);
    Ok(tokens.token)
}
