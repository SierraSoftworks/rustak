//! `GET /api/v1/me`: who the caller is, as far as this server is concerned.
//! `POST /api/v1/me/oidc-link`: hand that account to the identity provider.
//!
//! The channels are read per request rather than carried in the token, so that
//! removing somebody from one takes effect at once instead of when their token
//! expires. That is the same reason the administrative flag is read from the
//! account rather than believed from the `scope` claim.

use std::sync::Arc;

use actix_web::{HttpRequest, web};
use rustak_api::{AuditOutcome, TokenExchangeRequest, UserPreferencesPatch};

use crate::auth::RateLimiter;
use crate::identity::users;
use crate::prelude::*;
use crate::web::helpers::request::client_address;

use super::auth::{SUBJECT, not_configured, record, too_many, verify_code};
use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Authenticated;

/// Answers with the caller's identity.
///
/// # Errors
///
/// A `500` when the channels cannot be read.
pub async fn me(context: web::Data<AppContext>, caller: Authenticated) -> ApiResult {
    let me = users::me(context.db(), &caller.user, &caller.principal)
        .await
        .map_err(|err| {
            context.session().record_human_error(&err);
            ApiError::from_human(&err)
        })?;

    Ok(json_ok(&me))
}

/// `POST /me/oidc-link`: binds the caller's account to the identity the
/// provider just vouched for, and answers with who they now are.
///
/// The body is the same authorization-code exchange as `POST /auth/token`; the
/// difference is that the code decides which *identity* this is, and the
/// session decides which *account* it is bound to. That is what makes the link
/// safe without `link_by_username`: the person has proved they hold both.
///
/// # Errors
///
/// A `404` when no provider is configured, `429` when the caller has been
/// failing, `403` when `user_acl` refuses the identity, `400` when the exchange
/// is refused or the identity cannot be linked to this account, and `500` when
/// a write fails.
/// `PATCH /api/v1/me/preferences` — change what the caller has chosen about
/// how the console looks to them, and answer all of it.
///
/// Anybody signed in may: these are the account's own, and there is no route
/// by which one account reaches another's.
///
/// # Errors
///
/// A `400` when the patch names nothing, and a `500` when the write fails.
pub async fn preferences(
    context: web::Data<AppContext>,
    caller: Authenticated,
    patch: web::Json<UserPreferencesPatch>,
) -> ApiResult {
    if patch.is_empty() {
        return Err(ApiError::bad_request("That change would do nothing."));
    }

    let preferences = context
        .db()
        .user_preferences()
        .apply(caller.user.id, patch.into_inner())
        .await
        .map_err(|err| {
            context.session().record_human_error(&err);
            ApiError::from_human(&err)
        })?;

    Ok(json_ok(&preferences))
}

pub async fn link_oidc(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    caller: Authenticated,
    body: web::Json<TokenExchangeRequest>,
) -> ApiResult {
    let config = context.config();
    let provider = config.auth.oidc.as_ref().ok_or_else(not_configured)?;
    let address = client_address(
        config.server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    // The same limiter as the sign-in, because this redeems codes the same way
    // and guessing at one is guessing at the other.
    limiter.check(address, SUBJECT).map_err(too_many)?;

    match link(&context, provider, &request, &caller, &body).await {
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

/// The link itself, so the rate limiter wraps one expression.
async fn link(
    context: &AppContext,
    provider: &crate::config::OidcConfig,
    request: &HttpRequest,
    caller: &Authenticated,
    body: &TokenExchangeRequest,
) -> ApiResult {
    let (identity, acl) = verify_code(context, provider, request, body).await?;

    if !acl.allowed {
        record(context, "link", AuditOutcome::Denied, &caller.user.username).await;

        return Err(ApiError::forbidden(
            "That identity is not permitted to sign in to this server, so it cannot be linked.",
        ));
    }

    let row = users::link_identity(context, provider, &caller.user, &identity, acl.is_admin)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    record(context, "link", AuditOutcome::Success, &row.username).await;

    // Who they are *now*: the link may have renamed them, changed their
    // channels, or recorded an administrative standing.
    let principal = users::principal(
        context.db(),
        &row,
        caller.principal.via.clone(),
        acl.is_admin,
    )
    .await
    .map_err(|err| ApiError::from_human(&err))?;
    let me = users::me(context.db(), &row, &principal)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    Ok(json_ok(&me))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{AuthVia, Me};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn the_caller_is_described_with_the_channels_they_hold_now() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let me: Me = test::read_body_json(response).await;

        assert_eq!(me.username.as_str(), "ada");
        assert_eq!(me.via, AuthVia::Bearer);
        assert!(me.is_admin, "the account was created as an administrator");
        assert!(
            me.groups.iter().any(|held| held.group.is_anon()),
            "a new account is in the default channel",
        );
    }

    #[actix_web::test]
    async fn an_ordinary_account_is_not_reported_as_an_administrator() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let me: Me = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert!(!me.is_admin);
    }

    #[actix_web::test]
    async fn disabling_an_account_ends_its_session_at_once() {
        // The account is read per request rather than believed from the token,
        // which is the whole reason this takes effect before the token expires.
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", true).await;

        server
            .db()
            .users()
            .set_disabled(user.id, true)
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[cfg(test)]
mod link_tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{Me, User, UserSource};

    use crate::prelude::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;
    use crate::testing::oidc::{CODE, TestIdentityProvider};

    /// A server that trusts `provider`, lets anybody it vouches for sign in,
    /// and — the point — has *not* turned on `link_by_username`.
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
    async fn a_signed_in_person_links_their_own_account_and_keeps_their_session() {
        let provider = TestIdentityProvider::start_for("alice").await;
        let server = federated(&provider).await;
        let (before, session) = server.signed_in("alice", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/me/oidc-link")
                .insert_header(("authorization", bearer(&session)))
                .set_json(exchange())
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let me: Me = test::read_body_json(response).await;
        assert_eq!(me.username.as_str(), "alice");
        assert_eq!(me.source, UserSource::Oidc);
        assert_eq!(
            me.identity_provider.as_deref(),
            Some(provider.issuer().as_str())
        );
        assert!(me.is_admin, "the standing granted here survives the link");

        // The same session still works, and describes the linked account.
        let after: Me = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;
        assert_eq!(after.source, UserSource::Oidc);

        let row = server.db().users().get(before.id).await.unwrap().unwrap();
        assert_eq!(row.oidc_subject.as_deref(), Some("subject-id-for-alice"));
        assert_eq!(row.admin_override, Some(true));

        let users: Vec<User> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;
        assert_eq!(
            users.len(),
            1,
            "one account, not a second one for the provider"
        );
    }

    #[actix_web::test]
    async fn an_identity_that_is_already_somebody_else_cannot_be_linked() {
        let provider = TestIdentityProvider::start_for("alice").await;
        let server = federated(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        // The provider's "alice" signs in first and gets her own account.
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(exchange())
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        // Then somebody else, signed in with a passkey, tries to claim her.
        let (_, session) = server.signed_in("mallory", false).await;
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/me/oidc-link")
                .insert_header(("authorization", bearer(&session)))
                .set_json(exchange())
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn an_installation_with_no_provider_has_nothing_to_link_to() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("alice", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/me/oidc-link")
                .insert_header(("authorization", bearer(&session)))
                .set_json(exchange())
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
