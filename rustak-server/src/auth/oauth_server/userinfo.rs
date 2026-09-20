//! `GET|POST /oauth/userinfo` — who the token in front of us belongs to.
//!
//! The endpoint a relying party calls after the code exchange to find out whose
//! sign-in it just completed. CloudTAK reads `preferred_username` first and
//! falls back to `email`, and maps its own roles from `groups`; a standard
//! library reads whatever it was configured to.
//!
//! # Bearer only
//!
//! An `Authorization: Bearer` header and nothing else. Never a cookie: this is
//! a cross-origin endpoint a relying party's back end calls, so a cookie would
//! be both useless to it and a cross-site request forgery surface for a
//! browser, which is exactly the rule [`super::cookies`] already draws around
//! `/api/v1`. `POST` is accepted beside `GET` because RFC 5.3.1 allows it and
//! some libraries prefer it; neither changes anything.
//!
//! # Why the full claim set, whatever was asked for
//!
//! OpenID Connect says userinfo is narrowed to the scopes granted at the
//! authorization endpoint. rustak records those against the **code**, and a
//! code is spent in seconds — there is nowhere on a rustak access token to
//! carry them, and inventing a table to remember them would be new state with
//! its own lifetime and its own way to go wrong.
//!
//! It buys nothing either way. Every token that reaches here carries rustak's
//! `api` scope, and `GET /api/v1/me` answers the same four facts — username,
//! display name, email address and channels — to the same token. Narrowing this
//! endpoint while that one stays open would be a formality rather than a
//! control. What *is* enforced is the thing that matters: the token has to be
//! live, unrevoked, and belong to an account this installation still admits.

use actix_web::http::StatusCode;
use actix_web::http::header::{CACHE_CONTROL, HeaderValue, WWW_AUTHENTICATE};
use actix_web::{HttpRequest, HttpResponse, web};

use crate::auth::resolve::{RequestFacts, bearer};
use crate::marti::response;
use crate::prelude::*;
use crate::web::api::middleware::bearer_token;
use crate::web::helpers::request::client_ip;

use super::claims;

/// `GET|POST /oauth/userinfo`.
pub async fn userinfo(request: HttpRequest, context: web::Data<AppContext>) -> HttpResponse {
    let Some(token) = bearer_token(request.headers()) else {
        return unauthenticated("A userinfo request needs an access token.");
    };

    let config = context.config();
    let facts = RequestFacts {
        method: request.method().as_str(),
        path: request.path(),
        client_ip: client_ip(
            config.server.trust_proxy,
            request.headers(),
            request.peer_addr(),
        )
        .map(|ip| ip.to_string()),
        headers: request.headers(),
    };

    let resolved = match bearer(context.get_ref(), token, &facts).await {
        Ok(resolved) => resolved,
        Err(failure) => {
            debug!(reason = ?failure, "Refused a userinfo request.");

            return unauthenticated("That access token was not accepted.");
        }
    };

    let released = claims::released(
        context.db(),
        &config.auth.oauth,
        &resolved.user,
        resolved.principal.is_admin,
        None,
    )
    .await;

    let claims = match released {
        Ok(claims) => claims,
        Err(err) => {
            error!(error = %err, "Could not describe an account for userinfo.");
            context.session().record_human_error(&err);

            return response::bare_json_with(
                StatusCode::SERVICE_UNAVAILABLE,
                &serde_json::json!({ "error": "temporarily_unavailable" }),
            );
        }
    };

    let mut response = response::bare_json(&serde_json::Value::Object(claims));

    // Claims change when somebody is moved between channels, and a cached copy
    // is a stale answer to "who is this".
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));

    response
}

/// The one refusal a caller with no usable token gets.
///
/// `WWW-Authenticate: Bearer` as RFC 6750 §3 requires, so a library knows what
/// to present rather than guessing. The `error_description` says nothing about
/// *which* check failed — expired, revoked, forged and "that account is gone"
/// are one answer, because the difference is an oracle.
fn unauthenticated(description: &str) -> HttpResponse {
    let mut response = response::bare_json_with(
        StatusCode::UNAUTHORIZED,
        &serde_json::json!({ "error": "invalid_token", "error_description": description }),
    );

    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer error=\"invalid_token\""),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));

    response
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::CONTENT_TYPE;
    use actix_web::test::TestRequest;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer as bearer_header;

    #[actix_web::test]
    async fn a_request_with_no_token_is_told_what_to_present() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response =
            test::call_service(&app, TestRequest::get().uri("/oauth/userinfo").to_request()).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            response
                .headers()
                .get(WWW_AUTHENTICATE)
                .is_some_and(|value| value.as_bytes().starts_with(b"Bearer")),
            "RFC 6750 §3: a library has to be told what to present",
        );
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&response::JSON),
            "exactly application/json, with no charset parameter",
        );
    }

    #[actix_web::test]
    async fn a_token_this_server_did_not_issue_is_refused_the_same_way() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for value in ["Bearer not.a.token", "Bearer ", "Basic YWRhOng="] {
            let response = test::call_service(
                &app,
                TestRequest::get()
                    .uri("/oauth/userinfo")
                    .insert_header(("authorization", value))
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{value}");
        }
    }

    #[actix_web::test]
    async fn a_session_cookie_is_not_a_credential_here() {
        // This is a cross-origin endpoint a relying party's back end calls, so
        // a cookie would be useless to it and a forgery surface for a browser.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            TestRequest::get()
                .uri("/oauth/userinfo")
                .insert_header(("cookie", format!("access_token_0={}", session.token)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_live_token_describes_the_account_it_belongs_to() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            TestRequest::post()
                .uri("/oauth/userinfo")
                .insert_header(("authorization", bearer_header(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(body["sub"], "ada");
        assert_eq!(body["preferred_username"], "ada");
        assert!(
            body["groups"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "__ANON__")),
            "{body}",
        );
        assert!(
            body.get("email").is_none(),
            "an account with no email address must not be given one: {body}",
        );
    }

    #[actix_web::test]
    async fn a_revoked_token_stops_describing_anybody() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let claims = server.jwt().unwrap().verify(&session.token).unwrap();

        crate::auth::tokens::revoke(
            &server.context,
            &claims.jti,
            chrono::DateTime::from_timestamp(claims.exp, 0).unwrap(),
            user.id,
        )
        .await
        .unwrap();

        let response = test::call_service(
            &app,
            TestRequest::get()
                .uri("/oauth/userinfo")
                .insert_header(("authorization", bearer_header(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
