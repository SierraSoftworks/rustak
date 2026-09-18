//! What a browser session does once it exists: describe itself, hand its token
//! to the page, and end.
//!
//! Three of these four endpoints are TAK Server's, reproduced in its shapes so
//! that a WebTAK-style page written against a real server works here unchanged:
//! `/login/authserver` names the sign-in button, `/login/.well-known/
//! openid-configuration` publishes the **upstream** provider's two endpoints
//! (not ours — that path is a TAK invention and its contents are the identity
//! provider's), and `/token/access` hands a page the token its own cookies
//! already carry.
//!
//! `/logout` is the fourth. It answers `204` rather than TAK Server's `301` to
//! `/webtak/index.html`: there is no such page here, and a redirect from a
//! sign-out is a page nobody asked for.

use actix_web::http::StatusCode;
use actix_web::http::header::{CACHE_CONTROL, SET_COOKIE};
use actix_web::{HttpRequest, HttpResponse, web};

use crate::auth::resolve::{ListenerAuthPolicy, resolve_principal};
use crate::auth::tokens;
use crate::marti::response::{self, kind};
use crate::prelude::*;
use crate::web::api::middleware::bearer_token;
use crate::web::helpers::oidc;

use super::cookies;

/// `GET /login/authserver` — the name of the sign-in button.
pub async fn authserver(context: web::Data<AppContext>) -> HttpResponse {
    let config = context.config();

    let Some(provider) = config.auth.oidc.as_ref() else {
        // TAK Server answers `404` when no authorization server is configured,
        // and its clients read that as "this server has no single sign-on"
        // rather than as an error.
        return no_provider();
    };

    response::ok(kind::STRING, provider.display_name().to_string())
}

/// `GET /login/.well-known/openid-configuration` — the upstream provider's two
/// endpoints, in TAK Server's bare shape.
pub async fn openid_configuration(context: web::Data<AppContext>) -> HttpResponse {
    let config = context.config();

    let Some(provider) = config.auth.oidc.as_ref() else {
        return no_provider();
    };

    match oidc::discovery(context.get_ref(), provider).await {
        Ok(discovery) => response::bare_json(&serde_json::json!({
            "authorization_endpoint": discovery.authorization_endpoint,
            "token_endpoint": discovery.token_endpoint,
        })),
        Err(err) => {
            warn!(error = %err, "Could not describe the identity provider.");
            context.session().record_human_error(&err);

            refused()
        }
    }
}

/// `GET /token/access` — the caller's own access token, for a page that has to
/// present it to something else.
pub async fn token_access(request: HttpRequest, context: web::Data<AppContext>) -> HttpResponse {
    let config = context.config();

    if !config.auth.allow_access_token_retrieval {
        return response::bare_json_with(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "error": "access_denied" }),
        );
    }

    // Resolved rather than echoed: a caller presenting a token we would not
    // accept must not be handed it back with this server's blessing.
    let Ok(_) = resolve_principal(context.get_ref(), &request, ListenerAuthPolicy::public()).await
    else {
        return unauthenticated();
    };

    let Some(token) = presented(&request) else {
        return unauthenticated();
    };

    let mut response = response::ok(kind::STRING, token);

    response
        .headers_mut()
        .insert(CACHE_CONTROL, no_store_value());

    response
}

/// `GET|POST /logout` — end this session and clear its cookies.
pub async fn logout(request: HttpRequest, context: web::Data<AppContext>) -> HttpResponse {
    if let Ok(resolved) =
        resolve_principal(context.get_ref(), &request, ListenerAuthPolicy::public()).await
    {
        // The `jti` is what revocation lists, and the refresh family goes with
        // it: a sign-out that left a refresh token alive would be a sign-out
        // the browser could undo.
        let expires_at = resolved
            .token()
            .and_then(|claims| chrono::DateTime::from_timestamp(claims.exp, 0))
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(1));
        let jti = resolved
            .token()
            .map(|claims| claims.jti.clone())
            .unwrap_or_default();

        if let Err(err) =
            tokens::revoke(context.get_ref(), &jti, expires_at, resolved.user.id).await
        {
            warn!(error = %err, "Could not revoke a session that was signing out.");
            context.session().record_human_error(&err);
        }
    }

    let mut response = HttpResponse::NoContent().finish();

    response
        .headers_mut()
        .insert(CACHE_CONTROL, no_store_value());

    for value in cookies::clearing_cookies(request.headers()) {
        if let Ok(header) = actix_web::http::header::HeaderValue::from_str(&value) {
            response.headers_mut().append(SET_COOKIE, header);
        }
    }

    response
}

/// The token this request presented, from either place one may arrive.
fn presented(request: &HttpRequest) -> Option<String> {
    bearer_token(request.headers())
        .map(str::to_string)
        .or_else(|| cookies::access_token_from_cookies(request.headers()))
}

/// What an installation with no identity provider answers.
pub fn no_provider() -> HttpResponse {
    response::bare_json_with(
        StatusCode::NOT_FOUND,
        &serde_json::json!({ "error": "not_configured" }),
    )
}

/// The one refusal every failed sign-in gets. See [`super::login`].
pub fn refused() -> HttpResponse {
    let mut response = response::bare_json_with(
        StatusCode::BAD_REQUEST,
        &serde_json::json!({
            "error": "invalid_request",
            "error_description": "That sign-in could not be completed. Please start again.",
        }),
    );

    response
        .headers_mut()
        .insert(CACHE_CONTROL, no_store_value());

    response
}

/// [`refused`], with the half-finished flow's cookie taken away.
///
/// The `state` cookie is cleared on every failure whatever caused it, so that a
/// browser that hit one is not left holding a value a later callback could be
/// matched against.
pub fn callback_refused(request: &HttpRequest) -> HttpResponse {
    let mut response = refused();

    for value in cookies::clearing_cookies(request.headers()) {
        if let Ok(header) = actix_web::http::header::HeaderValue::from_str(&value) {
            response.headers_mut().append(SET_COOKIE, header);
        }
    }

    response
}

/// What a caller with no session is told.
fn unauthenticated() -> HttpResponse {
    let mut response = response::bare_json_with(
        StatusCode::UNAUTHORIZED,
        &serde_json::json!({ "error": "invalid_token" }),
    );

    response
        .headers_mut()
        .insert(CACHE_CONTROL, no_store_value());

    response
}

/// `no-store`, as a header value.
fn no_store_value() -> actix_web::http::header::HeaderValue {
    actix_web::http::header::HeaderValue::from_static("no-store")
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::CONTENT_TYPE;
    use actix_web::test::TestRequest;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn an_installation_with_no_provider_has_no_sign_in_button() {
        let server = TestServer::start().await;

        let response = authserver(web::Data::new(server.context.clone())).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            &response::JSON
        );
    }

    #[actix_web::test]
    async fn signing_out_clears_the_cookies_it_was_sent() {
        let server = TestServer::start().await;
        let request = TestRequest::post()
            .uri("/logout")
            .insert_header(("cookie", "access_token_0=a; state=b"))
            .app_data(web::Data::new(server.context.clone()))
            .to_http_request();

        let response = logout(request, web::Data::new(server.context.clone())).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let cleared: Vec<String> = response
            .headers()
            .get_all(SET_COOKIE)
            .filter_map(|value| value.to_str().ok().map(str::to_string))
            .collect();

        assert!(
            cleared
                .iter()
                .any(|value| value.starts_with("access_token_0=")),
            "{cleared:?}",
        );
        assert!(cleared.iter().any(|value| value.starts_with("state=")));
    }

    #[actix_web::test]
    async fn signing_out_stops_the_token_it_was_asked_with() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            TestRequest::post()
                .uri("/logout")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let after = test::call_service(
            &app,
            TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            after.status(),
            StatusCode::UNAUTHORIZED,
            "the token has to stop working before it expires",
        );
    }

    #[actix_web::test]
    async fn a_page_can_read_the_token_its_own_cookies_carry() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            TestRequest::get()
                .uri("/token/access")
                .insert_header(("cookie", format!("access_token_0={}", session.token)))
                .to_request(),
        )
        .await;

        assert_eq!(body["type"], kind::STRING);
        assert_eq!(body["data"], session.token);
    }

    #[actix_web::test]
    async fn an_installation_that_turned_it_off_hands_nobody_their_token() {
        let server = TestServer::start_with(|config| {
            config.auth.allow_access_token_retrieval = false;
        })
        .await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            TestRequest::get()
                .uri("/token/access")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn a_token_we_would_not_accept_is_not_handed_back_with_our_blessing() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            TestRequest::get()
                .uri("/token/access")
                .insert_header(("cookie", "access_token_0=not.a.token"))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
