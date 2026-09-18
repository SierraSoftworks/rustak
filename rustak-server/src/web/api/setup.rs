//! The first-run wizard.
//!
//! A fresh installation has nobody to authenticate as, so the first step is
//! authorised by a token written to the server's own filesystem: whoever can
//! read that file is whoever installed the server. Everything after it is
//! authorised by the session the wizard has established by then.
//!
//! # Why these routes disappear
//!
//! Once the wizard has been completed every route here answers `410`, for good.
//! Not `404`, which would be indistinguishable from a version that never had
//! them, and not a guard that only checks whether an administrator exists —
//! because deleting the last administrator must not reopen the door that
//! creates one without a credential.

use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, web};
use rustak_api::{
    AdminCreated, AuditCategory, AuditOutcome, CaSummary, CreateAdminRequest, InitCaRequest,
    ServerSettingsRequest, SetupStatus,
};

use crate::auth::{RateLimiter, setup};
use crate::db::AuditEntry;
use crate::identity::{settings, users};
use crate::pki;
use crate::prelude::*;
use crate::web::helpers::request::client_address;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;

/// The rate-limiter subject the setup token shares.
const SUBJECT: &str = "setup-token";

/// `GET /setup/status`: what the wizard still has to do.
///
/// Public, and deliberately so: the UI has to know whether to send somebody to
/// the wizard or to the sign-in page before anybody has signed in. It says what
/// has been done and nothing about how to do it.
///
/// # Errors
///
/// A `500` when the stored state cannot be read.
pub async fn status(context: web::Data<AppContext>) -> ApiResult {
    let db = context.db();
    let stored = settings::stored(db).await.map_err(failed)?;
    let has_admin = db.users().count_admins().await.map_err(failed)? > 0;
    let has_ca = db
        .get::<serde_json::Value>(pki::ca::PKI_PARTITION, pki::ca::ROOT_CA_KEY)
        .await
        .map_err(failed)?
        .is_some();
    let has_server_name = !settings::resolve(&context.config(), db)
        .await
        .map_err(failed)?
        .domains
        .is_empty();

    Ok(json_ok(&SetupStatus {
        needs_setup: !stored.is_setup_complete(),
        has_admin,
        has_ca,
        has_server_name,
        setup_completed: stored.is_setup_complete(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }))
}

/// `POST /setup/admin`: creates the first administrator.
///
/// Returns a short-lived registration token, because the account it just
/// created has no way to authenticate — the passkey is the thing being set up.
///
/// # Errors
///
/// `410` once the wizard is done, `409` once an administrator exists, `429`
/// when the caller has been guessing, `400` for a token that is not the one we
/// wrote, and `500` when a write fails.
pub async fn admin(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<CreateAdminRequest>,
) -> ApiResult {
    let config = context.config();
    let db = context.db();

    open(&context).await?;

    let address = client_address(
        config.server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    limiter.check(address, SUBJECT).map_err(too_many)?;

    if db.users().count_admins().await.map_err(failed)? > 0 {
        return Err(ApiError::conflict(
            "This server already has an administrator.",
        ));
    }

    if let Err(err) = setup::verify(db, &body.setup_token).await {
        limiter.record_failure(address, SUBJECT);
        audit(
            &context,
            "setup.admin",
            AuditOutcome::Denied,
            &body.username,
        )
        .await;

        return Err(ApiError::from_human(&err));
    }

    limiter.record_success(address, SUBJECT);

    let user = users::create_admin(
        db,
        body.username.clone(),
        body.display_name.clone(),
        body.email.clone(),
        config.auth.anon_group_default,
    )
    .await
    .map_err(|err| ApiError::from_human(&err))?;

    let registration_token = setup::issue_registration(db, user.id)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    info!(username = %user.username, "The first-run wizard created the first administrator.");
    audit(
        &context,
        "setup.admin",
        AuditOutcome::Success,
        &user.username,
    )
    .await;

    Ok(json_ok(&AdminCreated {
        username: user.username,
        registration_token,
        expires_in: (setup::REGISTRATION_TTL_MINUTES * 60) as u64,
    }))
}

/// `POST /setup/server`: records what this server is called and where it is.
///
/// # Errors
///
/// `410` once the wizard is done, `400` for a request naming no host, and
/// `500` when a write fails.
pub async fn server(
    context: web::Data<AppContext>,
    body: web::Json<ServerSettingsRequest>,
    caller: Administrative,
) -> ApiResult {
    open(&context).await?;

    let saved = settings::save(context.db(), &body, Some(&caller.user.username))
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    audit(
        &context,
        "setup.server",
        AuditOutcome::Success,
        &caller.user.username,
    )
    .await;

    Ok(json_ok(&saved))
}

/// `POST /setup/ca`: creates this installation's certificate authority.
///
/// # Errors
///
/// `410` once the wizard is done, `409` when an authority already exists — it
/// is what every enrolled device trusts, so replacing one is not something a
/// wizard step gets to do — and `500` when it cannot be created.
pub async fn ca(
    context: web::Data<AppContext>,
    body: web::Json<InitCaRequest>,
    caller: Administrative,
) -> ApiResult {
    open(&context).await?;

    let db = context.db();

    if db
        .get::<serde_json::Value>(pki::ca::PKI_PARTITION, pki::ca::ROOT_CA_KEY)
        .await
        .map_err(failed)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "This server already has a certificate authority.",
        ));
    }

    let config = context.config();
    let requested = crate::config::PkiConfig {
        ca_common_name: body.common_name.trim().to_string(),
        organization: body
            .organization
            .as_deref()
            .map(str::trim)
            .unwrap_or(&config.pki.organization)
            .to_string(),
        key_type: match body.key_type {
            rustak_api::CaKeyType::Rsa2048 => crate::config::KeyType::Rsa2048,
            rustak_api::CaKeyType::EcdsaP256 => crate::config::KeyType::EcdsaP256,
        },
        ..config.pki.clone()
    };

    let material =
        pki::load_or_create_root_ca(db, context.secrets(), &requested, &config.server.data_dir)
            .await
            .map_err(|err| ApiError::from_human(&err))?;

    audit(
        &context,
        "setup.ca",
        AuditOutcome::Success,
        &caller.user.username,
    )
    .await;

    Ok(json_ok(&CaSummary {
        subject: material.subject().to_string(),
        fingerprint: material.fingerprint().to_string(),
        not_before: material.not_before(),
        not_after: material.not_after(),
    }))
}

/// `POST /setup/complete`: closes the wizard, for good.
///
/// # Errors
///
/// `410` when it has already been closed, and `500` when the write fails.
pub async fn complete(context: web::Data<AppContext>, caller: Administrative) -> ApiResult {
    open(&context).await?;

    let db = context.db();

    settings::complete(db, Some(&caller.user.username))
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    setup::consume(db, &context.config().setup_token_file())
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    info!("The first-run wizard has been completed; its routes are closed.");
    audit(
        &context,
        "setup.completed",
        AuditOutcome::Success,
        &caller.user.username,
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Refuses every wizard route once the wizard is over.
async fn open(context: &AppContext) -> Result<(), ApiError> {
    if settings::stored(context.db())
        .await
        .map_err(failed)?
        .is_setup_complete()
    {
        return Err(ApiError::gone("This server has already been set up."));
    }

    Ok(())
}

/// Writes a wizard step to the audit log.
async fn audit(context: &AppContext, action: &'static str, outcome: AuditOutcome, who: &Username) {
    let entry = AuditEntry::new(AuditCategory::Administration, action, outcome).subject(who);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a setup step in the audit log.");
    }
}

/// Reports one of our own failures and generalises it.
fn failed(err: Error) -> ApiError {
    ApiError::from_human(&err)
}

/// The failure for somebody who has been guessing at the setup token.
fn too_many(retry_after: chrono::Duration) -> ApiError {
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
    use rustak_api::{AdminCreated, CaSummary, ServerSettings, SetupStatus};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    /// A server waiting to be set up, and the token it wrote out.
    async fn waiting() -> (TestServer, String) {
        let server = TestServer::start_with(|config| {
            // Nothing configured by hand, so the wizard is the only source of
            // a host name — which is the state this whole flow exists for.
            config.server.domains = Vec::new();
            config.pki.key_type = crate::config::KeyType::EcdsaP256;
        })
        .await;

        let token = setup::ensure(server.db(), &server.config().setup_token_file())
            .await
            .unwrap()
            .expect("a fresh installation writes a setup token")
            .token;

        (server, token)
    }

    fn admin_request(token: &str) -> serde_json::Value {
        serde_json::json!({
            "setup_token": token,
            "username": "ada",
            "display_name": "Ada Lovelace",
        })
    }

    #[actix_web::test]
    async fn a_fresh_installation_says_what_it_still_needs() {
        let (server, _) = waiting().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let status: SetupStatus = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/setup/status")
                .to_request(),
        )
        .await;

        assert!(status.needs_setup);
        assert!(!status.has_admin);
        assert!(!status.has_ca);
        assert!(!status.has_server_name);
        assert!(!status.setup_completed);
        assert_eq!(status.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[actix_web::test]
    async fn the_first_administrator_needs_the_token_the_server_wrote_out() {
        let (server, token) = waiting().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let refused = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/admin")
                .set_json(admin_request("not-the-token"))
                .to_request(),
        )
        .await;

        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            server.db().users().count_admins().await.unwrap(),
            0,
            "a refused token must not have created an account first",
        );

        let created: AdminCreated = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/admin")
                .set_json(admin_request(&token))
                .to_request(),
        )
        .await;

        assert_eq!(created.username.as_str(), "ada");
        assert!(!created.registration_token.is_empty());
        assert_eq!(server.db().users().count_admins().await.unwrap(), 1);
    }

    #[actix_web::test]
    async fn guessing_at_the_setup_token_is_rate_limited() {
        let server = TestServer::start_with(|config| {
            config.server.domains = Vec::new();
            config.auth.rate_limit.attempts = 3;
        })
        .await;

        setup::ensure(server.db(), &server.config().setup_token_file())
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;
        let mut statuses = Vec::new();

        for _ in 0..5 {
            statuses.push(
                test::call_service(
                    &app,
                    test::TestRequest::post()
                        .uri("/api/v1/setup/admin")
                        .set_json(admin_request("a-guess"))
                        .to_request(),
                )
                .await
                .status(),
            );
        }

        assert_eq!(statuses[0], StatusCode::BAD_REQUEST);
        assert_eq!(statuses[4], StatusCode::TOO_MANY_REQUESTS);
    }

    #[actix_web::test]
    async fn a_second_administrator_cannot_be_created_through_the_wizard() {
        let (server, token) = waiting().await;
        server.user("grace", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/admin")
                .set_json(admin_request(&token))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn the_wizard_records_the_host_name_and_mints_a_node_identifier() {
        let (server, _) = waiting().await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let saved: ServerSettings = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/server")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "name": "Hilltop TAK",
                    "domains": ["TAK.example.com"],
                }))
                .to_request(),
        )
        .await;

        assert_eq!(saved.domains, vec!["tak.example.com".to_string()]);
        assert!(saved.node_id.is_some());
    }

    #[actix_web::test]
    async fn the_wizard_creates_the_authority_once() {
        let (server, _) = waiting().await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let request = serde_json::json!({
            "common_name": "Hilltop CA",
            "organization": "Hilltop",
            "key_type": "ecdsa_p256",
        });

        let summary: CaSummary = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/ca")
                .insert_header(("authorization", bearer(&session)))
                .set_json(&request)
                .to_request(),
        )
        .await;

        assert!(summary.subject.starts_with("CN=Hilltop CA"));
        assert_eq!(summary.fingerprint.len(), 64);
        assert!(summary.not_after > summary.not_before);

        // Replacing one is not something a wizard step gets to do: it is what
        // every enrolled device trusts.
        let again = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/ca")
                .insert_header(("authorization", bearer(&session)))
                .set_json(&request)
                .to_request(),
        )
        .await;

        assert_eq!(again.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn every_wizard_route_is_gone_once_it_has_been_completed() {
        let (server, token) = waiting().await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let token_file = server.config().setup_token_file();

        let completed = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/complete")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(completed.status(), StatusCode::NO_CONTENT);
        assert!(
            !token_file.exists(),
            "the setup token file has to go when the wizard does",
        );

        for (uri, body) in [
            ("/api/v1/setup/admin", admin_request(&token)),
            (
                "/api/v1/setup/server",
                serde_json::json!({ "name": "x", "domains": ["x.example.com"] }),
            ),
            (
                "/api/v1/setup/ca",
                serde_json::json!({ "common_name": "x" }),
            ),
            ("/api/v1/setup/complete", serde_json::json!({})),
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri(uri)
                    .insert_header(("authorization", bearer(&session)))
                    .set_json(&body)
                    .to_request(),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::GONE,
                "{uri} was still reachable after the wizard finished",
            );
        }

        let status: SetupStatus = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/setup/status")
                .to_request(),
        )
        .await;

        assert!(status.setup_completed);
        assert!(!status.needs_setup);
    }

    #[actix_web::test]
    async fn deleting_the_last_administrator_does_not_reopen_the_wizard() {
        // A guard that only asked whether an administrator exists would hand
        // whoever emptied the table a way to create one without a credential.
        let (server, token) = waiting().await;
        let (admin, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/complete")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        server.db().users().delete(admin.id).await.unwrap();

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/admin")
                .set_json(admin_request(&token))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[actix_web::test]
    async fn an_ordinary_account_cannot_drive_the_wizard() {
        let (server, _) = waiting().await;
        let (_, session) = server.signed_in("grace", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/server")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "name": "x", "domains": ["x.example.com"] }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
