//! A thin client over the server's `/api/v1` routes.
//!
//! Authenticated calls attach the stored bearer token (see [`crate::auth`]).
//! When the server rejects one as expired (HTTP 401) the client renews it from
//! the stored refresh token and retries the request **once**; a 401 that
//! survives that is surfaced as [`ApiError::Unauthorized`] so the caller can
//! prompt for a fresh sign-in. Interactive sign-in is handled separately, in
//! [`crate::auth::oidc`] and [`crate::auth::passkey`].
//!
//! Requests are made relative to the current origin, so the same bundle works
//! behind any host name.
//!
//! # Demo mode
//!
//! This is the one place that knows about [`crate::fixtures`]. When the URL asks
//! for demo mode every call is served from an in-memory store instead of the
//! network, which is why no page needs a demo branch of its own — and why a page
//! cannot accidentally leave one out.

pub mod audit;
pub mod auth;
pub mod credentials;
pub mod devices;
pub mod groups;
pub mod health;
pub mod settings;
pub mod setup;
pub mod users;

use gloo_net::http::{Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::auth as session;

/// The base path of the admin API.
pub const API_BASE: &str = "/api/v1";

/// An error returned by an API call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The session is missing, expired, or could not be renewed.
    Unauthorized,

    /// The caller is not permitted to do this. Signing in again will not help.
    Forbidden,

    /// The wizard has already been completed, so its routes are gone.
    Gone,

    /// The request never produced a response.
    Network(String),

    /// The server answered with an error.
    Server(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "Your session has expired. Please sign in again."),
            ApiError::Forbidden => {
                write!(f, "Your account is not permitted to perform this action.")
            }
            ApiError::Gone => write!(f, "This server has already been set up."),
            ApiError::Network(message) => write!(f, "Network error: {message}"),
            ApiError::Server(message) => write!(f, "{message}"),
        }
    }
}

/// The HTTP verbs the client uses. A small enum so that a request can be
/// *rebuilt* for the retry that follows a token renewal.
#[derive(Clone, Copy)]
pub enum Verb {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// Builds a request with the bearer token (when there is one) and a JSON body.
fn build<B: Serialize>(
    verb: Verb,
    url: &str,
    token: Option<&str>,
    body: Option<&B>,
) -> Result<Request, ApiError> {
    let builder = match verb {
        Verb::Get => Request::get(url),
        Verb::Post => Request::post(url),
        Verb::Put => Request::put(url),
        Verb::Patch => Request::patch(url),
        Verb::Delete => Request::delete(url),
    };

    let builder = match token {
        Some(token) => builder.header("Authorization", &format!("Bearer {token}")),
        None => builder,
    };

    match body {
        Some(body) => builder
            .json(body)
            .map_err(|err| ApiError::Network(err.to_string())),
        None => builder
            .build()
            .map_err(|err| ApiError::Network(err.to_string())),
    }
}

/// Sends a request with the stored bearer token, renewing it once on a 401.
///
/// If the renewal fails the stored session is dropped and the original 401 is
/// returned, so the caller sees a refusal rather than a renewal failure — which
/// is the thing it can actually act on.
pub async fn send<B: Serialize>(
    verb: Verb,
    path: &str,
    body: Option<&B>,
) -> Result<Response, ApiError> {
    let url = format!("{API_BASE}{path}");
    let token = session::stored_token();
    let response = build(verb, &url, token.as_deref(), body)?
        .send()
        .await
        .map_err(|err| ApiError::Network(err.to_string()))?;

    if response.status() != 401 {
        return Ok(response);
    }

    if let Ok(fresh) = session::refresh_session().await {
        return build(verb, &url, Some(&fresh), body)?
            .send()
            .await
            .map_err(|err| ApiError::Network(err.to_string()));
    }

    session::clear_session();
    Ok(response)
}

/// The body the server sends with every error.
#[derive(serde::Deserialize)]
struct ServerError {
    error: String,
}

/// Converts a non-success response into an [`ApiError`], reading the JSON error
/// body when there is one.
pub async fn error_from_response(response: Response) -> ApiError {
    let status = response.status();
    match status {
        401 => return ApiError::Unauthorized,
        403 => return ApiError::Forbidden,
        410 => return ApiError::Gone,
        _ => {}
    }

    match response.json::<ServerError>().await {
        Ok(body) => ApiError::Server(body.error),
        Err(_) => ApiError::Server(format!(
            "The server returned an unexpected error ({status})."
        )),
    }
}

/// Reads a successful response as JSON, or converts the failure.
pub async fn json_response<T: DeserializeOwned>(response: Response) -> Result<T, ApiError> {
    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    response
        .json::<T>()
        .await
        .map_err(|err| ApiError::Server(err.to_string()))
}

/// GETs a path and deserialises the JSON body.
pub async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    json_response(send::<()>(Verb::Get, path, None).await?).await
}

/// POSTs a JSON body and deserialises the JSON response.
pub async fn post_json<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    json_response(send(Verb::Post, path, Some(body)).await?).await
}

/// POSTs a JSON body and expects no response body worth reading.
pub async fn post_empty<B: Serialize>(path: &str, body: &B) -> Result<(), ApiError> {
    let response = send(Verb::Post, path, Some(body)).await?;
    if response.ok() {
        Ok(())
    } else {
        Err(error_from_response(response).await)
    }
}

/// PATCHes a JSON body and deserialises the JSON response.
pub async fn patch_json<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    json_response(send(Verb::Patch, path, Some(body)).await?).await
}

/// PUTs a JSON body and deserialises the JSON response.
///
/// `PUT` is a replacement rather than a change: the caller sends the whole set
/// it wants, which is what makes "these are this account's channels" one
/// request instead of a grant and a revocation that could half-apply.
pub async fn put_json<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    json_response(send(Verb::Put, path, Some(body)).await?).await
}

/// DELETEs a path. The server answers `204`, so there is no body to read.
pub async fn delete_empty(path: &str) -> Result<(), ApiError> {
    let response = send::<()>(Verb::Delete, path, None).await?;
    if response.ok() {
        Ok(())
    } else {
        Err(error_from_response(response).await)
    }
}
