//! `/oauth/*` — the password grant CloudTAK's whole login hangs off.
//!
//! CloudTAK logs in by posting a username and a client password here and
//! reading the `access_token` out of the answer. Everything it does afterwards —
//! fetching the certificate config, enrolling, connecting the stream — depends
//! on this one exchange, which makes the shape of the response and the shape of
//! the token the most fragile surface in the project.
//!
//! # Three things that must not drift
//!
//! 1. **`Content-Type: application/json`, with no parameters.** node-tak
//!    compares the header with `===`; a `; charset=utf-8` suffix hands the
//!    caller a raw string that its own code then indexes into
//!    (`compat/cloudtak.md` §4).
//! 2. **Never a `3xx`.** node-tak reads anything under 400 as success and parses
//!    the redirect's empty body as the payload (§5).
//! 3. **No `refresh_token` in the password-grant response.** TAK Server issues
//!    none from this grant and CloudTAK's parser expects none; rustak's own
//!    rotating refresh tokens stay on `/api/v1/auth/refresh` and on the
//!    `refresh_token` grant here (`compat/oauth.md` §1).
//!
//! The token itself is pinned by [`crate::auth::jwt`]: a 27-byte header, so its
//! base64url encoding is a clean multiple of four characters, and flat scalar
//! claims, so CloudTAK's brace-splitting parser finds the payload. Neither is
//! cosmetic — see that module and `compat/oauth.md` §2.
//!
//! # Why a bad password is a `401` rather than a `400`
//!
//! OAuth 2.0 says `400 invalid_grant`, and TAK Server answers `401`. CloudTAK
//! accepts either and sniffs the body for `Bad credentials`; a `401` is also the
//! honest status for "that credential is not one we accept", so that is what is
//! emitted, with the substring CloudTAK looks for.

use actix_web::http::StatusCode;
use actix_web::{HttpRequest, web};
use std::sync::Arc;

use crate::auth::oauth_server::code_grant;
use crate::auth::oauth_server::responses::{
    TOKEN_TYPE, bad_credentials, expires_in, granted, oauth_error, rate_limited, unavailable,
};
use crate::auth::{RateLimiter, tokens};
use crate::identity::secret_cache::VerifiedSecretCache;
use crate::identity::verify::{Purpose, VerifyError, verify};
use crate::prelude::*;
use crate::web::helpers::request::client_address;

use super::error::MartiResult;
use super::response;

/// The rate-limiter subject used when a request named no account.
const ANONYMOUS_SUBJECT: &str = "oauth-token";

/// The form `POST /oauth/token` accepts.
///
/// `scope` is deliberately absent: neither verified client sends it and TAK
/// Server ignores it when it arrives. It is tolerated rather than rejected —
/// see [`token`].
#[derive(Clone, Deserialize)]
pub struct TokenForm {
    /// `password`, `refresh_token` or `authorization_code`.
    pub grant_type: String,

    #[serde(default)]
    pub username: Option<String>,

    #[serde(default)]
    pub password: Option<String>,

    #[serde(default)]
    pub refresh_token: Option<String>,

    /// The `authorization_code` grant: the code from `/oauth/authorize`.
    #[serde(default)]
    pub code: Option<String>,

    /// The `authorization_code` grant: the URI the code was delivered to,
    /// repeated so it can be compared with the one it was issued for.
    #[serde(default)]
    pub redirect_uri: Option<String>,

    /// The `authorization_code` grant: the proof-key verifier whose `S256`
    /// hash was registered when the code was issued.
    #[serde(default)]
    pub code_verifier: Option<String>,

    /// The `authorization_code` grant: which registered client is asking.
    ///
    /// Read for that grant alone. The password grant ignores it, because
    /// neither verified client sends one and TAK Server ignores it when they
    /// do (`compat/oauth.md` §1).
    #[serde(default)]
    pub client_id: Option<String>,

    /// The `authorization_code` grant: `client_secret_post`, for a confidential
    /// client that does not use `client_secret_basic`.
    ///
    /// Read for that grant alone, and never logged or traced: the field is
    /// deliberately not in this struct's `Debug` output, which is why
    /// [`TokenForm`] is never rendered with one.
    #[serde(default)]
    pub client_secret: Option<String>,
}

/// `POST /oauth/token`.
///
/// # Errors
///
/// Never as a [`MartiError`](super::error::MartiError): every refusal is an
/// OAuth error object with the status the specification (or TAK Server) gives
/// it, because a client parsing this endpoint expects `{"error": …}` and not
/// the Marti envelope.
pub async fn token(
    request: HttpRequest,
    context: web::Data<AppContext>,
    limiter: Option<web::Data<Arc<RateLimiter>>>,
    form: Option<web::Form<TokenForm>>,
) -> MartiResult {
    // A body we could not read at all is `invalid_request`, not a `400` from
    // actix's own extractor — which would answer text/plain and break a client
    // that insists on JSON.
    let Some(form) = form else {
        return Ok(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request body must be a form with a grant_type.",
        ));
    };

    let limiter = limiter.as_ref().map(|data| &**data.get_ref());

    match form.grant_type.as_str() {
        "password" => password_grant(&request, &context, limiter, &form).await,
        "refresh_token" => refresh_grant(&context, &form).await,
        "authorization_code" => Ok(code_grant::grant(
            &request,
            &context,
            limiter,
            code_grant::Form {
                code: form.code.as_deref(),
                redirect_uri: form.redirect_uri.as_deref(),
                client_id: form.client_id.as_deref(),
                client_secret: form.client_secret.as_deref(),
                code_verifier: form.code_verifier.as_deref(),
            },
        )
        .await),
        other => {
            debug!(grant = %other, "Refused a grant type this endpoint does not serve.");

            Ok(oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "This server supports the password, refresh_token and authorization_code grants.",
            ))
        }
    }
}

/// `GET /oauth/token_key` — the public key, in the Spring shape TAK tooling
/// reads.
///
/// # Errors
///
/// Never; an installation with no signing keys answers `503`, because that is a
/// start-up ordering problem rather than anything the caller did.
pub async fn token_key(context: web::Data<AppContext>) -> MartiResult {
    let Ok(jwt) = context.jwt() else {
        return Ok(unavailable());
    };

    Ok(response::bare_json(&jwt.token_key_json()))
}

/// `GET /oauth/jwks` — the same key as a JSON Web Key Set, plus any retired key
/// whose tokens could still be presented.
///
/// The set a relying party verifies an ID token against, so it is deliberately
/// cacheable: a library fetches it on the first token it sees and again when a
/// `kid` it does not know turns up, and an hour is short enough that a rotation
/// is not felt while being long enough that a busy relying party is not
/// refetching it per sign-in. Nothing here is secret — that is what makes a
/// public cache safe, and why the key material never leaves
/// [`crate::auth::jwt`].
///
/// # Errors
///
/// As [`token_key`].
pub async fn jwks(context: web::Data<AppContext>) -> MartiResult {
    let Ok(jwt) = context.jwt() else {
        return Ok(unavailable());
    };

    let mut response = response::bare_json(&jwt.jwks());

    response.headers_mut().insert(
        actix_web::http::header::CACHE_CONTROL,
        actix_web::http::header::HeaderValue::from_static("public, max-age=3600"),
    );

    Ok(response)
}

/// `grant_type=password`: a username and a client password for a token.
async fn password_grant(
    request: &HttpRequest,
    context: &AppContext,
    limiter: Option<&RateLimiter>,
    form: &TokenForm,
) -> MartiResult {
    let (Some(username), Some(password)) = (form.username.as_deref(), form.password.as_deref())
    else {
        return Ok(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "A password grant needs a username and a password.",
        ));
    };

    let address = client_address(
        context.config().server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );
    let subject = if username.is_empty() {
        ANONYMOUS_SUBJECT
    } else {
        username
    };

    // Rate limited before the username is even parsed, so that a flood of
    // malformed names costs the caller the same as a flood of real ones.
    let Some(limiter) = limiter else {
        error!("The token endpoint is mounted on a listener with no rate limiter.");

        return Ok(unavailable());
    };

    if let Err(retry_after) = limiter.check(address, subject) {
        return Ok(rate_limited(retry_after));
    }

    let Ok(parsed) = Username::parse(username) else {
        limiter.record_failure(address, subject);

        return Ok(bad_credentials());
    };

    let cache = VerifiedSecretCache::shared();

    let verified = match verify(
        context.db(),
        &parsed,
        password,
        Purpose::OAuthPassword,
        cache,
    )
    .await
    {
        Ok(verified) => verified,
        Err(VerifyError::Unavailable(err)) => {
            error!(error = %err, "Could not check a password grant.");
            context.session().record_human_error(&err);

            return Ok(unavailable());
        }
        Err(refusal) => {
            debug!(reason = refusal.reason(), "Refused a password grant.");
            limiter.record_failure(address, subject);

            return Ok(bad_credentials());
        }
    };

    limiter.record_success(address, subject);

    // Ordinary, non-consuming: a client password is reusable by design, and
    // recording the use is what lets an administrator see it being used.
    if let Err(err) =
        crate::identity::credentials::record_use(context.db(), &verified.credential, false, cache)
            .await
    {
        debug!(error = %err, "Could not record the use of a client password.");
    }

    let Ok(jwt) = context.jwt() else {
        return Ok(unavailable());
    };

    // Deliberately narrow, and never `admin`, whoever the account is. A client
    // password is the one reusable secret rustak issues and it is configured
    // into a TAK client rather than typed by a person at a sign-in page, so a
    // token minted from it stands for "this device may use the TAK surface",
    // not for everything its owner may do. `users::principal` treats the scope
    // as a ceiling, so this is what keeps a stolen client password out of the
    // administrative API (R-01 H1/H2). CloudTAK reads no `scope` from this
    // response and `compat/oauth.md` §1 pins the body's three fields, so
    // nothing on the wire changes.
    let scope = tokens::SCOPE_API;
    let issued = jwt.issue(&verified.user.username, scope, None, None);

    let (token, claims) = match issued {
        Ok(pair) => pair,
        Err(err) => {
            error!(error = %err, "Could not issue a token for a password grant.");
            context.session().record_human_error(&err);

            return Ok(unavailable());
        }
    };

    info!(
        username = %verified.user.username,
        "Issued an access token from a password grant.",
    );

    Ok(granted(serde_json::json!({
        "access_token": token,
        "token_type": TOKEN_TYPE,
        "expires_in": expires_in(claims.exp),
    })))
}

/// `grant_type=refresh_token`: rotating one of our own refresh tokens.
///
/// This grant is not one either verified client uses — it exists so that the
/// endpoint is a usable OAuth2 server rather than a single-purpose one — and it
/// *does* answer with a fresh refresh token, because rotation is the whole
/// point of it.
async fn refresh_grant(context: &AppContext, form: &TokenForm) -> MartiResult {
    let Some(presented) = form.refresh_token.as_deref() else {
        return Ok(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "A refresh_token grant needs a refresh_token.",
        ));
    };

    match tokens::rotate(context, presented, Some("oauth")).await {
        Ok(session) => Ok(granted(serde_json::json!({
            "access_token": session.token,
            "token_type": TOKEN_TYPE,
            "expires_in": session.expires_in,
            "refresh_token": session.refresh_token,
        }))),
        Err(err) => {
            debug!(error = %err, "Refused a refresh-token grant.");

            Ok(bad_credentials())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grants_this_endpoint_serves_are_named_in_its_refusal() {
        // A client that sent something else should be told what is on offer
        // rather than left guessing which of the three it wanted.
        let response = oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "This server supports the password, refresh_token and authorization_code grants.",
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn the_key_endpoints_say_so_when_no_key_is_installed() {
        // A start-up ordering problem rather than anything the caller did.
        let context = AppContext::new_mock(|_| {}).await.expect("a mock context");
        let data = web::Data::new(context);

        for response in [
            token_key(data.clone()).await.unwrap(),
            jwks(data.clone()).await.unwrap(),
        ] {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }
}
