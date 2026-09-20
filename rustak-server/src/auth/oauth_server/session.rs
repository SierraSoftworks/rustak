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
//! `/logout` is the fourth, and it is also the `end_session_endpoint` of
//! rustak's own discovery document. It answers `204` rather than TAK Server's
//! `301` to `/webtak/index.html`: there is no such page here, and a redirect
//! from a sign-out is a page nobody asked for.
//!
//! # The one redirect it will do
//!
//! A relying party may ask to be returned to itself afterwards, with
//! `client_id` and `post_logout_redirect_uri`. That is answered with a `302`
//! **only** when the URI is registered on that client under
//! `[auth.oauth] clients … post_logout_redirect_uris`, byte for byte, and with
//! a `204` in every other case — an unregistered URI, a URI on a client that
//! registered none, a `post_logout_redirect_uri` with no `client_id`, an
//! unknown `client_id`. A sign-out endpoint that redirected wherever it was
//! told is an open redirector on a path every session ends at, and the
//! specification's own "registered" requirement is the whole of the control.
//!
//! `id_token_hint` is **not** read. The specification allows a provider to take
//! the client from a token's `aud` instead of from `client_id`, and doing so
//! would mean parsing an attacker-supplied token before deciding where to send
//! a browser. Requiring `client_id` costs a relying party one query parameter.

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
///
/// # Why `GET` does less than `POST`
///
/// `GET /logout` is mounted because TAK clients navigate to it, and a top-level
/// navigation is exactly what `SameSite=Lax` still attaches the cookie to — so
/// any site can link to it. Signing *this* session out that way is a nuisance
/// the cookie is there for; taking every refresh family on the account with it
/// would let one link sign somebody out of every device they own (R-01 M2).
/// So the `GET` revokes the token that was presented, and only the `POST` —
/// which is same-site by construction — ends the account's other sessions.
pub async fn logout(
    request: HttpRequest,
    context: web::Data<AppContext>,
    query: Option<web::Query<LogoutQuery>>,
) -> HttpResponse {
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

        let ended = if request.method() == actix_web::http::Method::POST {
            tokens::revoke(context.get_ref(), &jti, expires_at, resolved.user.id).await
        } else {
            tokens::revoke_session(context.get_ref(), &jti, expires_at).await
        };

        if let Err(err) = ended {
            warn!(error = %err, "Could not revoke a session that was signing out.");
            context.session().record_human_error(&err);
        }
    }

    let mut response = match query
        .as_deref()
        .and_then(|query| returning_to(&context, query))
    {
        Some(location) => super::authorize::redirect(&location),
        None => HttpResponse::NoContent().finish(),
    };

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

/// The query `GET|POST /logout` accepts, all of it optional.
#[derive(Debug, Clone, Deserialize)]
pub struct LogoutQuery {
    /// Which registered client is asking to be returned to.
    #[serde(default)]
    pub client_id: Option<String>,

    /// Where it would like the browser sent, which has to be one of that
    /// client's registered `post_logout_redirect_uris`.
    #[serde(default)]
    pub post_logout_redirect_uri: Option<String>,

    /// The client's own state, returned untouched when there is a redirect.
    #[serde(default)]
    pub state: Option<String>,
}

/// Where a sign-out redirects to, when it redirects at all.
///
/// [`None`] — and therefore the `204` — for every case that is not "a
/// registered client asked to be returned to a URI it registered": see the
/// module documentation for why the answer is silence rather than an error.
fn returning_to(context: &AppContext, query: &LogoutQuery) -> Option<String> {
    let requested = query.post_logout_redirect_uri.as_deref()?;
    let client_id = query.client_id.as_deref()?;
    let config = context.config();
    let client = config.auth.oauth.client(client_id)?;

    if !client.allows_post_logout(requested) {
        warn!(
            client = %client_id,
            "Refused to return a sign-out to a URI the client is not registered for.",
        );

        return None;
    }

    match query.state.as_deref() {
        Some(state) => Some(format!(
            "{requested}{}state={}",
            if requested.contains('?') { '&' } else { '?' },
            super::authorize::encode(state),
        )),
        None => Some(requested.to_string()),
    }
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

        let response = logout(request, web::Data::new(server.context.clone()), None).await;

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

    /// A server with `app` registered, optionally with a sign-out URI.
    async fn with_client(post_logout: &[&str]) -> TestServer {
        let registered: Vec<String> = post_logout.iter().map(|uri| (*uri).to_string()).collect();

        TestServer::start_with(move |config| {
            config.auth.oauth = crate::config::OAuthServerConfig {
                clients: vec![crate::config::OAuthClient {
                    id: "app".to_string(),
                    redirect_uris: vec!["https://app.example.com/cb".to_string()],
                    public: true,
                    secret: None,
                    post_logout_redirect_uris: registered,
                }],
                ..crate::config::OAuthServerConfig::default()
            };
        })
        .await
    }

    /// `GET /logout` with `query` appended.
    async fn signing_out(server: &TestServer, query: &str) -> actix_web::dev::ServiceResponse {
        let app = test::init_service(App::new().configure(server.app())).await;

        test::call_service(
            &app,
            TestRequest::get()
                .uri(&format!("/logout{query}"))
                .to_request(),
        )
        .await
    }

    #[actix_web::test]
    async fn a_registered_sign_out_uri_is_returned_to_with_the_clients_state() {
        let server = with_client(&["https://app.example.com/bye"]).await;
        let response = signing_out(
            &server,
            "?client_id=app&post_logout_redirect_uri=https%3A%2F%2Fapp.example.com%2Fbye&state=a+b",
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::LOCATION)
                .unwrap(),
            "https://app.example.com/bye?state=a%20b",
        );
    }

    #[actix_web::test]
    async fn everything_else_signs_out_and_redirects_nowhere() {
        // A sign-out endpoint that redirected wherever it was told is an open
        // redirector on a path every session ends at.
        let server = with_client(&["https://app.example.com/bye"]).await;

        for query in [
            "",
            "?post_logout_redirect_uri=https%3A%2F%2Fapp.example.com%2Fbye",
            "?client_id=app&post_logout_redirect_uri=https%3A%2F%2Fevil.example.com%2Fbye",
            "?client_id=app&post_logout_redirect_uri=https%3A%2F%2Fapp.example.com%2Fbye%3Fx%3D1",
            "?client_id=nobody&post_logout_redirect_uri=https%3A%2F%2Fapp.example.com%2Fbye",
        ] {
            let response = signing_out(&server, query).await;

            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{query}");
            assert!(
                response
                    .headers()
                    .get(actix_web::http::header::LOCATION)
                    .is_none(),
                "{query}",
            );
        }
    }

    #[actix_web::test]
    async fn a_client_that_registered_no_sign_out_uri_is_never_redirected_to() {
        let server = with_client(&[]).await;
        let response = signing_out(
            &server,
            "?client_id=app&post_logout_redirect_uri=https%3A%2F%2Fapp.example.com%2Fcb",
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "a redirect URI is not a sign-out URI",
        );
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
