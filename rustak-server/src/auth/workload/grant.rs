//! `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer` — an assertion
//! exchanged for one of our access tokens.
//!
//! RFC 7523 §2.1: a client that already holds a signed statement of what it is
//! posts it as `assertion` and gets back an access token. For a sidecar under
//! an orchestrator that **replaces the service token entirely** — the control
//! API is reached with a token this exchange produced, and a deployment holds
//! no rustak secret to be copied out of a file.
//!
//! # What comes back is deliberately narrow
//!
//! [`tokens::SCOPE_API`] and never `admin`, whoever the account is. The same
//! argument as the password grant's: the credential was configured into a
//! machine rather than typed by a person at a sign-in page, so the token stands
//! for "this service may use its own API", not for everything its account could
//! do. `users::principal` treats the scope as a ceiling, so this is what keeps
//! a leaked assertion out of `/api/v1/users`.
//!
//! # The response is the password grant's
//!
//! `access_token`, `token_type`, `expires_in`, exactly `application/json`, no
//! `refresh_token` — the shape `compat/oauth.md` §1 pins and CloudTAK's parser
//! expects. A client that already reads the password grant reads this.

use actix_web::HttpRequest;
use actix_web::http::StatusCode;

use crate::auth::oauth_server::responses::{
    TOKEN_TYPE, expires_in, granted, oauth_error, rate_limited, unavailable,
};
use crate::auth::ratelimit::RateLimiter;
use crate::auth::resolve::{AuthFailure, RequestFacts};
use crate::auth::tokens;
use crate::prelude::*;
use crate::web::helpers::request::client_address;

/// The grant type, spelled as RFC 7523 §2.1 spells it.
pub const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// What a refusal says when there is nothing more specific to say.
///
/// Everything about the *credential* — which check it failed, and by how much —
/// goes back in `error_description`, because the caller is the token's holder
/// and telling them their own token expired an hour ago is the difference
/// between a two-hour outage and a one-line diagnosis. Everything about this
/// *installation* — whether the account exists, whether it is switched off,
/// what `user_acl` says — falls back to this one sentence, because that
/// difference is an oracle.
const REFUSED: &str = "That assertion was not accepted.";

/// Exchanges a workload assertion for a rustak access token.
///
/// # Errors
///
/// Never as an [`Error`]: every refusal is an OAuth error object with the
/// status the specification gives it, because a client parsing this endpoint
/// expects `{"error": …}`.
#[instrument("auth.workload.grant", skip_all)]
pub async fn jwt_bearer(
    request: &HttpRequest,
    context: &AppContext,
    limiter: Option<&RateLimiter>,
    assertion: Option<&str>,
) -> actix_web::HttpResponse {
    let Some(token) = assertion.filter(|value| !value.is_empty()) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "A jwt-bearer grant needs an `assertion`.",
        );
    };

    // An unlimited credential endpoint is worse than one that is briefly
    // unavailable, so a listener that installed no limiter refuses outright.
    let Some(limiter) = limiter else {
        error!("The token endpoint is mounted on a listener with no rate limiter.");

        return unavailable();
    };

    let address = client_address(
        context.config().server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    let facts = RequestFacts {
        method: request.method().as_str(),
        path: request.path(),
        client_ip: address.map(|ip| ip.to_string()),
        headers: request.headers(),
    };

    // `Deliberately`: the caller asked for the `jwt-bearer` grant by name, so
    // anything wrong with what came with it is worth an operator's attention.
    let resolved = match super::resolve_limited(
        context,
        limiter,
        address,
        token,
        &facts,
        super::Presented::Deliberately,
    )
    .await
    {
        Ok((resolved, _)) => resolved,
        Err(denied) => {
            // The sentence goes back to the caller, because the caller is the
            // one holding the token it is about; see `Denied::description`.
            let description = denied.description();

            return match AuthFailure::from(denied) {
                AuthFailure::RateLimited(retry_after) => rate_limited(retry_after),
                AuthFailure::Unavailable(err) => {
                    error!(error = %err, "Could not check a jwt-bearer grant.");
                    context.session().record_human_error(&err);

                    unavailable()
                }
                _ => oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    &description.unwrap_or_else(|| REFUSED.to_string()),
                ),
            };
        }
    };

    let Ok(jwt) = context.jwt() else {
        return unavailable();
    };

    // Never `admin`; see the module documentation.
    let issued = jwt.issue(&resolved.user.username, tokens::SCOPE_API, None, None);

    let (token, claims) = match issued {
        Ok(pair) => pair,
        Err(err) => {
            error!(error = %err, "Could not issue a token for a jwt-bearer grant.");
            context.session().record_human_error(&err);

            return unavailable();
        }
    };

    info!(
        account = %resolved.user.username,
        "Issued an access token from a workload identity.",
    );

    granted(serde_json::json!({
        "access_token": token,
        "token_type": TOKEN_TYPE,
        "expires_in": expires_in(claims.exp),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn the_grant_type_is_spelled_the_way_rfc_7523_spells_it() {
        // Copied by hand into deployments; a typo here is a grant nobody can
        // reach and an `unsupported_grant_type` nobody can explain.
        assert_eq!(GRANT_TYPE, "urn:ietf:params:oauth:grant-type:jwt-bearer");
    }

    #[actix_web::test]
    async fn a_grant_with_no_assertion_is_invalid_request_rather_than_invalid_grant() {
        let server = crate::testing::TestServer::start().await;
        let request = TestRequest::post().uri("/oauth/token").to_http_request();

        for assertion in [None, Some("")] {
            let response = jwt_bearer(&request, &server.context, None, assertion).await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[actix_web::test]
    async fn an_endpoint_with_no_rate_limiter_refuses_rather_than_guessing_freely() {
        let server = crate::testing::TestServer::start().await;
        let request = TestRequest::post().uri("/oauth/token").to_http_request();

        let response = jwt_bearer(&request, &server.context, None, Some("a.b.c")).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn an_assertion_nobody_registered_an_issuer_for_is_invalid_grant() {
        let server = crate::testing::TestServer::start().await;
        let request = TestRequest::post().uri("/oauth/token").to_http_request();

        let response = jwt_bearer(
            &request,
            &server.context,
            Some(&server.limiter),
            Some("a.b.c"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
