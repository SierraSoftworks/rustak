//! Signing in through an identity provider, renewing, and signing out.
//!
//! The browser runs the authorization request itself and hands us the code; we
//! hold the client secret, redeem the code, verify the ID token, decide who
//! that is and issue **our own** session. The provider's token is never a
//! credential for this API, which is what lets one token format serve the admin
//! UI, CloudTAK and sidecars alike.
//!
//! # What `user_acl` gates
//!
//! This is where it is evaluated, because this is where the provider's claims
//! exist. It denies everybody by default, which is deliberate: an installation
//! that has configured a provider but not said who from it may in should let
//! nobody in rather than everybody. Passkeys are not gated by it — a passkey
//! was registered against an account that already exists here, so the decision
//! was made when it was registered.

use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, web};
use rustak_api::{
    AuditCategory, AuditOutcome, AuthMetadata, AuthMode, TokenExchangeRequest, TokenRefreshRequest,
};

use crate::auth::acl::{AclOutcome, AuthRequestFilter, evaluate};
use crate::auth::{RateLimiter, tokens};
use crate::db::AuditEntry;
use crate::identity::users;
use crate::prelude::*;
use crate::web::helpers::oidc;
use crate::web::helpers::request::{client_address, client_ip};

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Authenticated;

/// The rate-limiter subject the token endpoints share.
pub(super) const SUBJECT: &str = "auth-token";

/// `GET /auth/metadata`: what sign-in methods this installation offers.
///
/// A provider that cannot be reached falls back to reporting passkeys rather
/// than failing. That is not papering over the outage: passkeys exist partly so
/// that an administrator keeps a way in when the directory is down, and a login
/// page that refuses to render is the one thing that would take it away.
pub async fn metadata(context: web::Data<AppContext>) -> HttpResponse {
    let config = context.config();

    let Some(provider) = config.auth.oidc.as_ref() else {
        return json_ok(&AuthMetadata::passkeys_only());
    };

    match oidc::discovery(context.get_ref(), provider).await {
        Ok(discovery) => json_ok(&AuthMetadata {
            mode: AuthMode::Oidc {
                authorization_endpoint: discovery.authorization_endpoint,
                client_id: provider.client_id.clone(),
                scopes: provider.scopes(),
                pkce: true,
            },
            passkeys_enabled: true,
        }),
        Err(err) => {
            warn!(error = %err, "The identity provider is unreachable; offering passkeys instead.");
            context.session().record_human_error(&err);

            json_ok(&AuthMetadata::passkeys_only())
        }
    }
}

/// `POST /auth/token`: exchanges an authorization code for one of our sessions.
///
/// # Errors
///
/// A `404` when no provider is configured, `429` when the caller has been
/// failing, `403` when `user_acl` refuses them, `400` when the exchange or the
/// token is refused, and `500` when a write fails.
pub async fn token(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<TokenExchangeRequest>,
) -> ApiResult {
    let config = context.config();
    let provider = config.auth.oidc.as_ref().ok_or_else(not_configured)?;
    let address = client_address(
        config.server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    limiter.check(address, SUBJECT).map_err(too_many)?;

    let outcome = sign_in(&context, provider, &request, &body).await;

    match outcome {
        Ok(response) => {
            limiter.record_success(address, SUBJECT);

            Ok(response)
        }
        Err(err) => {
            limiter.record_failure(address, SUBJECT);

            Err(err)
        }
    }
}

/// The sign-in itself, so the rate limiter wraps one expression.
async fn sign_in(
    context: &AppContext,
    provider: &crate::config::OidcConfig,
    request: &HttpRequest,
    body: &TokenExchangeRequest,
) -> ApiResult {
    let config = context.config();
    let (identity, acl) = verify_code(context, provider, request, body).await?;

    if !acl.allowed {
        record(context, "login", AuditOutcome::Denied, &identity.username).await;

        return Err(ApiError::forbidden(
            "Your account is not permitted to sign in to this server.",
        ));
    }

    let user = users::provision(
        context.db(),
        provider,
        &identity,
        acl.is_admin,
        config.auth.anon_group_default,
    )
    .await
    .map_err(|err| ApiError::from_human(&err))?;

    let is_admin = user.is_effective_admin() || acl.is_admin;
    let session = tokens::issue_session(context, &user, is_admin, Some("admin-ui"))
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    record(context, "login", AuditOutcome::Success, &user.username).await;

    Ok(json_ok(&session))
}

/// Redeems an authorization code and decides who the provider vouched for.
///
/// Shared by the sign-in and by `POST /me/oidc-link`, which does everything a
/// sign-in does up to the point of deciding which account the identity
/// belongs to. The access-control outcome is returned rather than enforced,
/// because the two callers refuse in different words and audit under
/// different names.
///
/// # Errors
///
/// `502` when the provider cannot be reached, and `400` when the exchange or
/// the token is refused.
pub(super) async fn verify_code(
    context: &AppContext,
    provider: &crate::config::OidcConfig,
    request: &HttpRequest,
    body: &TokenExchangeRequest,
) -> Result<(users::VerifiedIdentity, AclOutcome), ApiError> {
    let config = context.config();
    let discovery = oidc::discovery(context, provider).await.map_err(|err| {
        context.session().record_human_error(&err);

        ApiError::new(
            actix_web::http::StatusCode::BAD_GATEWAY,
            "We could not reach your identity provider.",
        )
    })?;

    let tokens_from_provider = oidc::exchange_code(
        &context.http_client(),
        provider,
        &discovery,
        &body.code,
        &body.redirect_uri,
        body.code_verifier.as_deref(),
    )
    .await
    .map_err(|err| ApiError::from_human(&err))?;

    // The nonce is `None` here because the browser runs the authorization
    // request itself and binds the flow with proof key for code exchange
    // instead. The server-driven `/login/*` flow TAK clients use does issue
    // one, and checks it in `oauth_server::login` against the `PendingAuth` it
    // recorded — a nonce this endpoint has nothing to compare against.
    let claims = oidc::validate_token(context, provider, &tokens_from_provider.id_token, None)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    let identity = oidc::identity_from_claims(provider, &discovery.issuer, &claims)
        .map_err(|err| ApiError::from_human(&err))?;

    let filterable = oidc::filterable_claims(&claims);
    let acl = evaluate(
        &config.auth,
        &AuthRequestFilter {
            method: request.method().as_str(),
            path: request.path(),
            client_ip: client_ip(
                config.server.trust_proxy,
                request.headers(),
                request.peer_addr(),
            ),
            headers: request.headers(),
            claims: Some(&filterable),
            username: identity.username.as_str(),
            source: "oidc",
        },
    );

    Ok((identity, acl))
}

/// `POST /auth/refresh`: exchanges a refresh token for a new pair.
///
/// # Errors
///
/// `429` when the caller has been failing, `401` when the token is not one we
/// would renew, and `500` when a write fails.
pub async fn refresh(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<TokenRefreshRequest>,
) -> ApiResult {
    let config = context.config();
    let address = client_address(
        config.server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    limiter.check(address, SUBJECT).map_err(too_many)?;

    match tokens::rotate(context.get_ref(), &body.refresh_token, Some("admin-ui")).await {
        Ok(session) => {
            limiter.record_success(address, SUBJECT);

            Ok(json_ok(&session))
        }
        Err(err) => {
            limiter.record_failure(address, SUBJECT);

            Err(ApiError::unauthorized(err.description()))
        }
    }
}

/// `POST /auth/logout`: ends this session.
///
/// # Errors
///
/// A `500` when the revocation cannot be recorded.
pub async fn logout(context: web::Data<AppContext>, caller: Authenticated) -> ApiResult {
    // Every `/api/v1` session is a bearer token, so this is always present; a
    // request that reached here another way has no `jti` to revoke and is
    // answered by revoking nothing rather than by failing.
    let Some(claims) = caller.token() else {
        return Ok(HttpResponse::NoContent().finish());
    };

    let expires_at = chrono::DateTime::from_timestamp(claims.exp, 0).unwrap_or_else(
        // A token whose expiry we cannot read is still one to list; an hour is
        // longer than any we issue.
        || chrono::Utc::now() + chrono::Duration::hours(1),
    );

    tokens::revoke(context.get_ref(), &claims.jti, expires_at, caller.user.id)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    record(
        context.get_ref(),
        "logout",
        AuditOutcome::Success,
        &caller.user.username,
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Writes a sign-in, sign-out or link to the audit log.
pub(super) async fn record(
    context: &AppContext,
    action: &'static str,
    outcome: AuditOutcome,
    who: &Username,
) {
    let entry = AuditEntry::new(AuditCategory::Authentication, action, outcome).subject(who);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a sign-in in the audit log.");
        context.session().record_human_error(&err);
    }
}

/// The failure for an installation with no identity provider.
pub(super) fn not_configured() -> ApiError {
    ApiError::not_found("This server does not federate to an identity provider.")
}

/// The failure for somebody who has been guessing.
pub(super) fn too_many(retry_after: chrono::Duration) -> ApiError {
    ApiError::new(
        actix_web::http::StatusCode::TOO_MANY_REQUESTS,
        format!(
            "Too many attempts. Try again in {} minutes.",
            retry_after.num_minutes().max(1)
        ),
    )
    .with_code("rate_limited")
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{AuthMetadata, AuthMode, TokenResponse, User};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;
    use crate::testing::oidc::{CODE, TestIdentityProvider};

    /// A server that trusts `provider` and lets anybody it vouches for sign in.
    async fn federated(provider: &TestIdentityProvider) -> TestServer {
        let oidc = provider.config();

        TestServer::start_with(move |config| {
            config.auth.oidc = Some(oidc);
            config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
        })
        .await
    }

    fn exchange() -> serde_json::Value {
        serde_json::json!({
            "code": CODE,
            "redirect_uri": "https://localhost/auth/callback",
            "code_verifier": "a-verifier-the-browser-kept",
        })
    }

    #[actix_web::test]
    async fn an_installation_with_no_provider_offers_passkeys() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let metadata: AuthMetadata = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/metadata")
                .to_request(),
        )
        .await;

        assert_eq!(metadata.mode, AuthMode::Passkey);
        assert!(metadata.passkeys_enabled);
    }

    #[actix_web::test]
    async fn a_federated_installation_tells_the_browser_where_to_send_somebody() {
        let provider = TestIdentityProvider::start().await;
        let server = federated(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let metadata: AuthMetadata = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/metadata")
                .to_request(),
        )
        .await;

        match metadata.mode {
            AuthMode::Oidc {
                authorization_endpoint,
                client_id,
                scopes,
                pkce,
            } => {
                assert_eq!(
                    authorization_endpoint,
                    format!("{}/authorize", provider.issuer())
                );
                assert_eq!(client_id, crate::testing::oidc::CLIENT_ID);
                assert_eq!(scopes.first().map(String::as_str), Some("openid"));
                assert!(pkce, "the browser is never left to assume it");
            }
            other => panic!("expected a federated installation, got {other:?}"),
        }

        assert!(
            metadata.passkeys_enabled,
            "a passkey is how an administrator gets in when the directory is down",
        );
    }

    #[actix_web::test]
    async fn a_provider_that_is_down_leaves_passkeys_rather_than_a_broken_login_page() {
        let server = TestServer::start_with(|config| {
            let mut oidc = crate::config::OidcConfig {
                endpoint: "http://127.0.0.1:1".to_string(),
                client_id: "rustak".to_string(),
                client_secret: "secret".to_string(),
                scopes: Vec::new(),
                username_claim: "preferred_username".to_string(),
                groups_claim: "groups".to_string(),
                group_prefix: String::new(),
                strip_group_prefix: true,
                read_suffix: "_READ".to_string(),
                write_suffix: "_WRITE".to_string(),
                read_only_group: None,
                auto_create_groups: true,
                link_by_username: false,
                display_name: None,
            };
            oidc.scopes.push("email".to_string());
            config.auth.oidc = Some(oidc);
        })
        .await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let metadata: AuthMetadata = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/metadata")
                .to_request(),
        )
        .await;

        assert_eq!(metadata.mode, AuthMode::Passkey);
    }

    #[actix_web::test]
    async fn the_endpoint_we_publish_is_one_a_browser_can_actually_be_sent_to() {
        // The whole round trip a browser makes: ask where to go, go there, come
        // back with a code, hand it over. Testing the exchange alone would pass
        // against metadata that named an endpoint nobody could use.
        let provider = TestIdentityProvider::start().await;
        let server = federated(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let metadata: AuthMetadata = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/metadata")
                .to_request(),
        )
        .await;

        let AuthMode::Oidc {
            authorization_endpoint,
            client_id,
            ..
        } = metadata.mode
        else {
            panic!("a federated installation should offer its provider");
        };

        let mut authorize = url::Url::parse(&authorization_endpoint).unwrap();
        authorize
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", "https://localhost/auth/callback")
            .append_pair("state", "the-browsers-state")
            .append_pair("code_challenge_method", "S256");

        let redirect = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(authorize)
            .send()
            .await
            .unwrap();

        let location = redirect
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .expect("the provider sends the browser back");
        let returned = url::Url::parse(location).unwrap();
        let code = returned
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.to_string())
            .expect("the redirect carries a code");

        assert_eq!(
            returned
                .query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.to_string())
                .as_deref(),
            Some("the-browsers-state"),
            "the state has to come back untouched, or the browser cannot match it",
        );

        let session: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(serde_json::json!({
                    "code": code,
                    "redirect_uri": "https://localhost/auth/callback",
                    "code_verifier": "the-verifier-the-browser-kept",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(
            server.jwt().unwrap().verify(&session.token).unwrap().sub,
            "alice"
        );
    }

    #[actix_web::test]
    async fn signing_in_creates_the_account_and_hands_back_one_of_our_own_sessions() {
        let provider = TestIdentityProvider::start().await;
        let server = federated(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let session: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(exchange())
                .to_request(),
        )
        .await;

        let claims = server.jwt().unwrap().verify(&session.token).unwrap();
        assert_eq!(
            claims.sub, "alice",
            "the session is ours, issued to the account the provider named",
        );
        assert!(session.refresh_token.is_some());

        let me: rustak_api::Me = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(me.username.as_str(), "alice");
        assert!(
            me.groups.iter().any(|held| held.group.as_str() == "ops"),
            "the provider's group claim is mapped to a channel at sign-in",
        );
    }

    #[actix_web::test]
    async fn an_installation_that_has_not_said_who_may_in_lets_nobody_in() {
        // `user_acl` denies by default, and this is the endpoint where that
        // decision is made — a federated installation that never wrote one
        // should let nobody in rather than everybody.
        let provider = TestIdentityProvider::start().await;
        let oidc = provider.config();
        let server = TestServer::start_with(move |config| {
            config.auth.oidc = Some(oidc);
        })
        .await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(exchange())
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            server
                .db()
                .users()
                .list(crate::db::Page::first(10))
                .await
                .unwrap()
                .is_empty(),
            "a refused sign-in must not have created an account first",
        );
    }

    #[actix_web::test]
    async fn the_expression_decides_who_administers_the_installation() {
        let provider = TestIdentityProvider::start().await;
        let oidc = provider.config();
        let server = TestServer::start_with(move |config| {
            config.auth.oidc = Some(oidc);
            config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
            config.auth.admin_acl =
                Some(filt_rs::Filter::new(r#"claims.groups contains "ops_WRITE""#).unwrap());
        })
        .await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let session: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(exchange())
                .to_request(),
        )
        .await;

        let users: Vec<User> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(users.len(), 1);
    }

    #[actix_web::test]
    async fn a_code_the_provider_will_not_redeem_is_a_failure_rather_than_a_session() {
        let provider = TestIdentityProvider::start().await;
        let server = federated(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(serde_json::json!({
                    "code": "not-the-code-it-issued",
                    "redirect_uri": "https://localhost/auth/callback",
                    "code_verifier": "a-verifier",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn the_provider_refuses_an_exchange_that_carries_no_verifier() {
        // Proof key for code exchange is not decoration: a code intercepted
        // from the redirect has to be worthless without it, and the only way to
        // know we send it is to have a provider that insists.
        let provider = TestIdentityProvider::start().await;
        let server = federated(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(serde_json::json!({
                    "code": CODE,
                    "redirect_uri": "https://localhost/auth/callback",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn an_installation_with_no_provider_has_no_code_to_exchange() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(exchange())
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_session_can_be_renewed_and_a_spent_token_cannot() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let refresh = session.refresh_token.clone().unwrap();

        let renewed: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/refresh")
                .set_json(serde_json::json!({ "refresh_token": refresh }))
                .to_request(),
        )
        .await;

        assert!(server.jwt().unwrap().verify(&renewed.token).is_ok());

        let replayed = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/refresh")
                .set_json(serde_json::json!({ "refresh_token": refresh }))
                .to_request(),
        )
        .await;

        assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn guessing_at_refresh_tokens_is_rate_limited() {
        let server = TestServer::start_with(|config| {
            config.auth.rate_limit.attempts = 3;
        })
        .await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let mut statuses = Vec::new();

        for _ in 0..5 {
            statuses.push(
                test::call_service(
                    &app,
                    test::TestRequest::post()
                        .uri("/api/v1/auth/refresh")
                        .set_json(serde_json::json!({ "refresh_token": "a-guess" }))
                        .to_request(),
                )
                .await
                .status(),
            );
        }

        assert_eq!(statuses[0], StatusCode::UNAUTHORIZED);
        assert_eq!(
            statuses[4],
            StatusCode::TOO_MANY_REQUESTS,
            "guessing has to cost time, or a refresh token is only as good as the guesses allowed",
        );
    }

    #[actix_web::test]
    async fn signing_out_ends_the_session_it_was_asked_from() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/logout")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let after = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            after.status(),
            StatusCode::UNAUTHORIZED,
            "the access token has to stop working before it expires",
        );

        let renewal = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/refresh")
                .set_json(serde_json::json!({
                    "refresh_token": session.refresh_token.clone().unwrap(),
                }))
                .to_request(),
        )
        .await;

        assert_eq!(renewal.status(), StatusCode::UNAUTHORIZED);
    }
}
