//! Passkeys: `navigator.credentials` on this side, `webauthn-rs` on the other.
//!
//! There are no passwords in rustak, so this is how somebody signs in when there
//! is no identity provider, and how the first administrator is bootstrapped by
//! the wizard.
//!
//! # Why the payloads are translated rather than modelled
//!
//! The options the server sends and the credential the browser returns are
//! WebAuthn's own structures. `webauthn-rs` produces and consumes them at one
//! end and the browser does at the other, and neither asks this crate to
//! understand them — so they travel as `serde_json::Value` (see
//! `rustak_api::passkey`) and all this module does is move them across the
//! JavaScript boundary.
//!
//! That boundary is not free, though: WebAuthn's JSON form carries binary
//! fields as base64url **strings**, while `navigator.credentials` wants
//! `ArrayBuffer`s going in and hands `ArrayBuffer`s back. So `create` and `get`
//! decode on the way in (`to_buffer`, `descriptor_ids`) and re-encode on the way
//! out (`encode`). Every field that needs converting is named explicitly rather
//! than guessed at by shape, because a string that silently stayed a string is a
//! ceremony that fails inside the browser with nothing to read.

use base64::prelude::*;
use js_sys::{Array, Object, Reflect, Uint8Array};
use rustak_api::{
    PasskeyLoginFinish, PasskeyRegistrationFinish, PasskeyRegistrationStart, TokenResponse,
    Username,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AuthenticatorAssertionResponse, AuthenticatorAttestationResponse, CredentialCreationOptions,
    CredentialRequestOptions, PublicKeyCredential,
};

use crate::api;
use crate::auth::store_session;
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::window;

/// Registers a new passkey.
///
/// `registration_token` is the wizard's short-lived grant, used exactly once for
/// the first administrator — who cannot be signed in yet, because the passkey
/// they are registering is what would sign them in. Everybody else is already
/// authenticated and passes `None`.
///
/// Returns the session when the server chose to establish one, so the wizard can
/// carry on without a second prompt.
pub async fn register(
    label: &str,
    registration_token: Option<String>,
) -> Result<Option<TokenResponse>, String> {
    let challenge = api::auth::passkey_register_start(&PasskeyRegistrationStart {
        label: label.to_string(),
        registration_token,
    })
    .await
    .map_err(|err| err.to_string())?;

    let credential = create(&challenge.options).await?;

    let tokens = api::auth::passkey_register_finish(&PasskeyRegistrationFinish {
        challenge_id: challenge.challenge_id,
        credential,
        label: Some(label.to_string()),
    })
    .await
    .map_err(|err| err.to_string())?;

    if let Some(tokens) = &tokens {
        store_session(tokens);
    }
    Ok(tokens)
}

/// Signs in with a passkey, storing the session it establishes.
///
/// Omitting the username asks the authenticator which account it holds, which is
/// the better flow: naming an account before proving anything is what would let
/// somebody ask this server which accounts exist.
pub async fn login(username: Option<Username>) -> Result<(), String> {
    let challenge = api::auth::passkey_login_start(username)
        .await
        .map_err(|err| err.to_string())?;

    let credential = get(&challenge.options).await?;

    let tokens = api::auth::passkey_login_finish(&PasskeyLoginFinish {
        challenge_id: challenge.challenge_id,
        credential,
    })
    .await
    .map_err(|err| err.to_string())?;

    store_session(&tokens);
    Ok(())
}

/// Runs the registration ceremony and returns what the browser produced.
async fn create(options: &Value) -> Result<Value, String> {
    demo!(Ok(fixtures::demo_credential()));

    let public_key = js_options(options)?;
    to_buffer(&public_key, "challenge")?;
    to_buffer(&member(&public_key, "user")?, "id")?;
    descriptor_ids(&public_key, "excludeCredentials")?;

    let request = CredentialCreationOptions::new();
    request.set_public_key(&public_key.unchecked_into());

    let credential = await_credential(
        credentials()?
            .create_with_options(&request)
            .map_err(|_| refused("register"))?,
    )
    .await?;

    let response: AuthenticatorAttestationResponse = credential.response().unchecked_into();
    let transports: Vec<String> = response
        .get_transports()
        .iter()
        .filter_map(|value| value.as_string())
        .collect();

    let mut body = base_json(&credential)?;
    body["response"] = json!({
        "clientDataJSON": encode(&response.client_data_json()),
        "attestationObject": encode(&response.attestation_object()),
        "transports": transports,
    });
    Ok(body)
}

/// Runs the sign-in ceremony and returns what the browser produced.
async fn get(options: &Value) -> Result<Value, String> {
    demo!(Ok(fixtures::demo_credential()));

    let public_key = js_options(options)?;
    to_buffer(&public_key, "challenge")?;
    descriptor_ids(&public_key, "allowCredentials")?;

    let request = CredentialRequestOptions::new();
    request.set_public_key(&public_key.unchecked_into());

    let credential = await_credential(
        credentials()?
            .get_with_options(&request)
            .map_err(|_| refused("sign in"))?,
    )
    .await?;

    let response: AuthenticatorAssertionResponse = credential.response().unchecked_into();

    let mut body = base_json(&credential)?;
    body["response"] = json!({
        "clientDataJSON": encode(&response.client_data_json()),
        "authenticatorData": encode(&response.authenticator_data()),
        "signature": encode(&response.signature()),
        "userHandle": response.user_handle().as_ref().map(encode),
    });
    Ok(body)
}

fn credentials() -> Result<web_sys::CredentialsContainer, String> {
    let container = window().navigator().credentials();
    if container.is_undefined() {
        return Err("this browser does not support passkeys".to_string());
    }
    Ok(container)
}

/// The message shown when the browser or the authenticator declines.
///
/// Deliberately vague about the cause. The browser reports "the user cancelled"
/// and "no matching credential" as the same kind of error on purpose — telling
/// them apart would say whether an account has a passkey — and repeating a
/// guess here would undo that.
fn refused(action: &str) -> String {
    format!("the passkey prompt could not {action} — it may have been dismissed")
}

async fn await_credential(promise: js_sys::Promise) -> Result<PublicKeyCredential, String> {
    JsFuture::from(promise)
        .await
        .map_err(|_| refused("complete"))?
        .dyn_into::<PublicKeyCredential>()
        .map_err(|_| "the browser returned something that was not a passkey".to_string())
}

/// The fields every credential carries, whichever ceremony produced it.
///
/// `authenticatorAttachment` is read reflectively because `web-sys` only exposes
/// its accessor behind `web_sys_unstable_apis`, and turning that on for the whole
/// crate would be a large door opened for one optional string. It is absent on
/// older browsers, which is why the server treats it as optional anyway.
fn base_json(credential: &PublicKeyCredential) -> Result<Value, String> {
    let attachment = Reflect::get(credential, &JsValue::from_str("authenticatorAttachment"))
        .ok()
        .and_then(|value| value.as_string());

    Ok(json!({
        "id": credential.id(),
        "rawId": encode(&credential.raw_id()),
        "type": credential.type_(),
        "authenticatorAttachment": attachment,
        "extensions": {},
    }))
}

/// Parses the server's options into a live JavaScript object.
///
/// `webauthn-rs` wraps them in `publicKey`, matching what
/// `navigator.credentials` expects to be handed; a server that sends the inner
/// dictionary on its own is accepted too, because the difference is not worth a
/// failed ceremony.
fn js_options(options: &Value) -> Result<Object, String> {
    let inner = options.get("publicKey").unwrap_or(options);
    js_sys::JSON::parse(&inner.to_string())
        .map_err(|_| "the server's passkey options could not be read".to_string())?
        .dyn_into::<Object>()
        .map_err(|_| "the server's passkey options were not an object".to_string())
}

/// Reads a nested object, for the fields that have one.
fn member(parent: &Object, key: &str) -> Result<Object, String> {
    Reflect::get(parent, &JsValue::from_str(key))
        .map_err(|_| missing(key))?
        .dyn_into::<Object>()
        .map_err(|_| missing(key))
}

fn missing(key: &str) -> String {
    format!("the server's passkey options were missing `{key}`")
}

/// Replaces a base64url string field with the buffer WebAuthn wants.
fn to_buffer(parent: &Object, key: &str) -> Result<(), String> {
    let name = JsValue::from_str(key);
    let Some(encoded) = Reflect::get(parent, &name).ok().and_then(|v| v.as_string()) else {
        return Err(missing(key));
    };

    let bytes = decode(&encoded).ok_or_else(|| missing(key))?;
    Reflect::set(parent, &name, &Uint8Array::from(bytes.as_slice()))
        .map_err(|_| missing(key))
        .map(|_| ())
}

/// Does the same for the `id` of every entry in a credential-descriptor list,
/// which is optional — a server with nothing to exclude or allow omits it.
fn descriptor_ids(parent: &Object, key: &str) -> Result<(), String> {
    let Ok(list) = Reflect::get(parent, &JsValue::from_str(key)) else {
        return Ok(());
    };
    if !Array::is_array(&list) {
        return Ok(());
    }

    for entry in Array::from(&list).iter() {
        if let Ok(descriptor) = entry.dyn_into::<Object>() {
            to_buffer(&descriptor, "id")?;
        }
    }
    Ok(())
}

/// base64url without padding, which is the form WebAuthn's JSON uses.
fn encode(buffer: &js_sys::ArrayBuffer) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(Uint8Array::new(buffer).to_vec())
}

/// The inverse, forgiving about padding and about a server that reached for
/// standard base64 — both decode to the same bytes, and refusing one of them
/// would fail a ceremony over punctuation.
fn decode(value: &str) -> Option<Vec<u8>> {
    let trimmed = value.trim_end_matches('=');
    BASE64_URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| BASE64_STANDARD_NO_PAD.decode(trimmed))
        .ok()
}
