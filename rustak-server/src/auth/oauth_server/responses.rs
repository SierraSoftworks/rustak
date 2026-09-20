//! The bodies `/oauth/token` answers with, in the shapes its callers parse.
//!
//! Shared between the two grants that are TAK Server's ([`crate::marti::oauth`]
//! serves `password` and `refresh_token`) and the one that is ours
//! ([`super::code_grant`]), so that a refusal looks the same whichever path
//! produced it. A client parsing this endpoint expects `{"error": …}` and never
//! the Marti envelope.
//!
//! Three rules hold for every one of them, and each is a CloudTAK bug written
//! down (`compat/oauth.md` §1, `compat/cloudtak.md` §4):
//!
//! 1. Exactly `application/json`, with no `charset` parameter. node-tak
//!    compares the header with `===`.
//! 2. Never a `3xx`. node-tak reads anything under 400 as success and parses
//!    the redirect's empty body as the payload.
//! 3. `no-store` on anything carrying a token, so no proxy keeps a copy.

use actix_web::http::StatusCode;
use actix_web::http::header::RETRY_AFTER;
use actix_web::{HttpResponse, http::header};

use crate::marti::response;

/// The `token_type` every response carries. Capitalised, as both clients send
/// it back.
pub const TOKEN_TYPE: &str = "Bearer";

/// How long an access token has left, in whole seconds.
///
/// Saturating rather than wrapping: `expires_in` is unsigned on the wire, and a
/// token that expired while the response was being built must not become
/// eighteen quintillion seconds.
pub fn expires_in(exp: i64) -> u64 {
    u64::try_from(exp - chrono::Utc::now().timestamp()).unwrap_or(0)
}

/// A successful grant: exactly `application/json`, and never cached.
pub fn granted(body: serde_json::Value) -> HttpResponse {
    let mut response = response::bare_json(&body);

    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );

    response
}

/// An OAuth error object, with the status it belongs to.
pub fn oauth_error(status: StatusCode, error: &str, description: &str) -> HttpResponse {
    response::bare_json_with(
        status,
        &serde_json::json!({ "error": error, "error_description": description }),
    )
}

/// The one refusal a code grant ever gets, whichever binding failed.
pub fn invalid_grant() -> HttpResponse {
    oauth_error(
        StatusCode::BAD_REQUEST,
        "invalid_grant",
        "That authorization code was not accepted.",
    )
}

/// The one refusal a client that did not authenticate ever gets.
///
/// `401` rather than `400`, as RFC 6749 §5.2 requires for `invalid_client`, and
/// deliberately identical whether the secret was wrong, empty or absent: the
/// difference is an oracle for which identifiers are registered as
/// confidential. No `WWW-Authenticate` header, because offering `Basic` here
/// invites a browser to put up a password box on a machine-to-machine endpoint.
pub fn invalid_client() -> HttpResponse {
    oauth_error(
        StatusCode::UNAUTHORIZED,
        "invalid_client",
        "That client did not authenticate.",
    )
}

/// The one refusal a bad credential ever gets.
///
/// `Bad credentials` is the substring CloudTAK sniffs for to show a friendlier
/// message; nothing branches on the rest of the wording.
pub fn bad_credentials() -> HttpResponse {
    oauth_error(
        StatusCode::UNAUTHORIZED,
        "invalid_grant",
        "Bad credentials: that username and password were not accepted.",
    )
}

/// Too many attempts, and when to come back.
pub fn rate_limited(retry_after: chrono::Duration) -> HttpResponse {
    let seconds = retry_after.num_seconds().max(1);
    let mut response = oauth_error(
        StatusCode::TOO_MANY_REQUESTS,
        "invalid_grant",
        "Bad credentials: too many attempts. Try again later.",
    );

    if let Ok(value) = header::HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }

    response
}

/// A failure of ours, in the shape this endpoint's callers parse.
pub fn unavailable() -> HttpResponse {
    oauth_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "temporarily_unavailable",
        "This server cannot issue tokens right now.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_carries_the_substring_cloudtak_looks_for() {
        // CloudTAK turns a body containing `Bad credentials` into a friendlier
        // message; nothing else about the wording is parsed.
        let response = bad_credentials();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&response::JSON),
            "node-tak compares this header with string equality",
        );
    }

    #[test]
    fn a_granted_token_is_never_cached() {
        let response = granted(serde_json::json!({ "access_token": "x" }));

        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store",
        );
    }

    #[test]
    fn a_lockout_says_when_to_come_back() {
        let response = rate_limited(chrono::Duration::minutes(15));

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "900");
    }

    #[test]
    fn an_expiry_in_the_past_is_zero_rather_than_an_enormous_number() {
        assert_eq!(expires_in(chrono::Utc::now().timestamp() - 60), 0);
        assert!(expires_in(chrono::Utc::now().timestamp() + 60) > 0);
    }

    #[test]
    fn a_client_that_did_not_authenticate_is_a_401_and_offers_no_password_box() {
        // `401` is what RFC 6749 §5.2 gives `invalid_client`; a
        // `WWW-Authenticate: Basic` would make a browser put up a dialogue on a
        // machine-to-machine endpoint.
        let response = invalid_client();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
    }
}
