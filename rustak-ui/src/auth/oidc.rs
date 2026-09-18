//! Signing in through the identity provider, in a popup, with PKCE.
//!
//! A browser client cannot hold a client secret, so the server performs the
//! confidential half of the exchange: the browser runs the authorization request
//! in a popup, the popup posts the resulting `code` (and the PKCE verifier) to
//! `/api/v1/auth/token`, and the server answers with **our** bearer and refresh
//! tokens.
//!
//! It is a popup rather than a redirect so the page behind it is never navigated
//! away from — a half-filled setup wizard survives a sign-in. The popup loads
//! this same SPA at the callback route, [`complete_callback`] does the exchange,
//! and the tokens are handed back to the opener through a short-lived
//! `localStorage` slot (a popup does not share `sessionStorage` with its
//! opener), after which it closes itself.
//!
//! Everything the browser talks to is same-origin, so the provider never has to
//! permit cross-origin requests.

use std::time::Duration;

use base64::prelude::*;
use rustak_api::{AuthMode, TokenExchangeRequest, TokenResponse};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::api;
use crate::auth::{
    POPUP_RESULT_KEY, STATE_KEY, VERIFIER_KEY, local, random_token, session, store_session,
};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::{urlencode, window};

/// The route the provider redirects to. It is registered with the provider, so
/// it is a constant rather than something a page chooses.
const CALLBACK_PATH: &str = "/auth/callback";

/// How long the opener waits between polls of the handoff slot, and for how many
/// of them (~10 minutes, which is long enough to find a second factor).
const POPUP_POLL_INTERVAL: Duration = Duration::from_millis(300);
const POPUP_MAX_POLLS: u32 = 2_000;

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

fn redirect_uri() -> String {
    format!("{}{CALLBACK_PATH}", origin())
}

/// Whether this window was opened as a sign-in popup, which decides whether
/// [`complete_callback`] hands its tokens back and closes or keeps them.
fn is_popup() -> bool {
    window()
        .opener()
        .map(|opener| !opener.is_null() && !opener.is_undefined())
        .unwrap_or(false)
}

/// The SHA-256 of a verifier, base64url-encoded: the `code_challenge` for PKCE.
async fn s256(verifier: &str) -> Result<String, String> {
    let bytes = verifier.as_bytes().to_vec();
    let promise = window()
        .crypto()
        .map_err(|_| "this browser has no Web Crypto")?
        .subtle()
        .digest_with_str_and_u8_array("SHA-256", &bytes)
        .map_err(|_| "this browser could not hash the PKCE verifier")?;

    let digest = JsFuture::from(promise)
        .await
        .map_err(|_| "hashing the PKCE verifier failed")?
        .dyn_into::<js_sys::ArrayBuffer>()
        .map_err(|_| "the PKCE digest was not a buffer")?;

    Ok(BASE64_URL_SAFE_NO_PAD.encode(js_sys::Uint8Array::new(&digest).to_vec()))
}

/// Builds the provider's authorization URL.
async fn authorize_url(mode: &AuthMode, state: &str) -> Result<String, String> {
    let AuthMode::Oidc {
        authorization_endpoint,
        client_id,
        scopes,
        pkce,
    } = mode
    else {
        return Err("this server has no identity provider configured".to_string());
    };

    // `openid` is what makes it an OpenID Connect request rather than a bare
    // OAuth one, so it is added whether or not the server listed it.
    let mut all = vec!["openid".to_string()];
    all.extend(scopes.iter().filter(|scope| *scope != "openid").cloned());

    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        authorization_endpoint,
        urlencode(client_id),
        urlencode(&redirect_uri()),
        urlencode(&all.join(" ")),
        urlencode(state),
    );

    if *pkce {
        let verifier = random_token(32).ok_or("this browser cannot generate a PKCE verifier")?;
        let challenge = s256(&verifier).await?;
        if let Some(storage) = session() {
            let _ = storage.set_item(VERIFIER_KEY, &verifier);
        }
        url.push_str(&format!(
            "&code_challenge={}&code_challenge_method=S256",
            urlencode(&challenge)
        ));
    }

    Ok(url)
}

fn callback_params() -> Option<(String, String)> {
    let search = window().location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    Some((params.get("code")?, params.get("state")?))
}

/// Begins an interactive sign-in and waits for the popup to report back.
///
/// `Ok(None)` means the popup was dismissed without completing, which is a
/// choice rather than a failure. Must be called from a user gesture, or the
/// browser blocks the popup.
pub async fn begin_login() -> Result<Option<String>, String> {
    demo!({
        store_session(&fixtures::sign_in_with_passkey());
        Ok(Some(fixtures::demo_token()))
    });

    let state = random_token(24).ok_or("this browser cannot generate a login state")?;
    if let Some(storage) = session() {
        let _ = storage.set_item(STATE_KEY, &state);
    }
    // Clear any stale handoff from an abandoned attempt before opening a popup
    // that is about to write to the same slot.
    if let Some(storage) = local() {
        let _ = storage.remove_item(POPUP_RESULT_KEY);
    }

    let metadata = api::auth::metadata().await.map_err(|err| err.to_string())?;
    let url = authorize_url(&metadata.mode, &state).await?;

    let blocked = || "the browser blocked the sign-in popup".to_string();
    let popup = window()
        .open_with_url_and_target_and_features(&url, "rustak-login", "popup,width=480,height=720")
        .map_err(|_| blocked())?
        .ok_or_else(blocked)?;

    for _ in 0..POPUP_MAX_POLLS {
        if let Some(result) = local().and_then(|s| s.get_item(POPUP_RESULT_KEY).ok().flatten()) {
            if let Some(storage) = local() {
                let _ = storage.remove_item(POPUP_RESULT_KEY);
            }
            let tokens: TokenResponse =
                serde_json::from_str(&result).map_err(|err| err.to_string())?;
            store_session(&tokens);
            return Ok(Some(tokens.token));
        }
        if popup.closed().unwrap_or(false) {
            return Ok(None);
        }
        gloo_timers::future::sleep(POPUP_POLL_INTERVAL).await;
    }

    Err("the sign-in popup did not complete in time".to_string())
}

/// Finishes a callback, if the current URL is one.
///
/// In a popup the tokens are handed back to the opener and the window closes
/// (returning `None`). On a direct navigation they are stored and the bearer is
/// returned. `None` also means there was no callback to process.
pub async fn complete_callback() -> Result<Option<String>, String> {
    let Some((code, state)) = callback_params() else {
        return Ok(None);
    };
    let storage = session().ok_or("session storage is unavailable")?;

    // The state is what ties this response to the request this tab made. A
    // mismatch is either a stale login or a forged one, and neither is worth
    // exchanging a code for.
    if storage.get_item(STATE_KEY).ok().flatten().as_deref() != Some(state.as_str()) {
        return Err("the sign-in response did not match this browser's request".into());
    }

    let request = TokenExchangeRequest {
        code,
        redirect_uri: redirect_uri(),
        code_verifier: storage.get_item(VERIFIER_KEY).ok().flatten(),
    };

    let tokens = api::auth::exchange_code(&request)
        .await
        .map_err(|err| err.to_string())?;

    let _ = storage.remove_item(STATE_KEY);
    let _ = storage.remove_item(VERIFIER_KEY);

    if is_popup() {
        if let Some(local) = local() {
            let serialised = serde_json::to_string(&tokens).map_err(|err| err.to_string())?;
            let _ = local.set_item(POPUP_RESULT_KEY, &serialised);
        }
        let _ = window().close();
        return Ok(None);
    }

    // A direct navigation: keep the tokens, and scrub the code and state out of
    // the address bar so that a shared or bookmarked URL carries neither.
    store_session(&tokens);
    if let Ok(history) = window().history() {
        let _ =
            history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(CALLBACK_PATH));
    }

    Ok(Some(tokens.token))
}
