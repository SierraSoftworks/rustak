//! `/api/v1/services`: the control API a sidecar registers and reports through.
//!
//! Thin, like every route file: parse, authenticate, call [`crate::plugins`],
//! render.
//! The rules — a name belongs to the account that claimed it, a service may read
//! only its own configuration, only an administrator may write one — are in
//! [`crate::plugins::registry`] and [`crate::plugins::auth`], where they can be
//! tested without
//! a request.
//!
//! # Why these routes sit outside the session gate
//!
//! Everything else under `/api/v1` is behind
//! [`api_auth`](super::middleware::api_auth), which accepts one of our own RS256
//! access tokens and nothing else. A sidecar has a *service token* — and, once
//! it has enrolled, a client certificate — neither of which that gate would let
//! through, so these handlers resolve their own caller through
//! [`crate::plugins::auth::caller`]. They are mounted in the same scope as the public
//! routes for that reason, and every one of them refuses an unauthenticated
//! request in its first three lines. `web::api`'s own route-table test asserts
//! that.

use actix_web::{HttpRequest, HttpResponse, web};
use rustak_api::{Heartbeat, ServiceDescriptor};

use crate::auth::resolve::AuthFailure;
use crate::db::repos::ServiceRow;
use crate::plugins::{Caller, RegistryError, auth, health, registry};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};

/// Registers the control-API routes.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/services", web::get().to(list))
        .route("/services/register", web::post().to(register))
        .route("/services/{name}", web::delete().to(remove))
        .route("/services/{name}/heartbeat", web::post().to(heartbeat))
        .route("/services/{name}/config", web::get().to(get_config))
        .route("/services/{name}/config", web::put().to(put_config));
}

/// `POST /api/v1/services/register`.
///
/// # Errors
///
/// A `401` without a usable credential, a `403` for a caller that is not a
/// service, a `409` when the name belongs to another account, and a `500` when a
/// write fails.
pub async fn register(
    context: web::Data<AppContext>,
    request: HttpRequest,
    body: web::Bytes,
) -> ApiResult {
    let caller = caller(&context, &request).await?;

    if !caller.is_service() {
        // A person has no service account to register *as*, so this would have
        // to invent one — and a registration nobody can authenticate as is a row
        // that only ever confuses whoever reads the listing.
        return Err(ApiError::forbidden(
            "Only a service may register itself. Create a service account and mint it a token.",
        ));
    }

    let descriptor: ServiceDescriptor = parse(&body, "a service descriptor")?;
    let row = registry::register(context.get_ref(), &caller.identity.user, &descriptor)
        .await
        .map_err(|err| failed(&context, err))?;

    Ok(json_ok(&registry::summary(&row)))
}

/// `GET /api/v1/services`.
///
/// # Errors
///
/// A `401` without a credential, a `403` for anybody but an administrator, and a
/// `500` when the read fails.
pub async fn list(context: web::Data<AppContext>, request: HttpRequest) -> ApiResult {
    let caller = caller(&context, &request).await?;
    caller.require_admin().map_err(refusal)?;

    let services = registry::list(context.get_ref())
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    Ok(json_ok(&services))
}

/// `POST /api/v1/services/{name}/heartbeat`.
///
/// Answers `404` when the registration has gone, which is what tells a running
/// sidecar to register again rather than to give up.
///
/// # Errors
///
/// A `401` without a credential, a `403` when the service is not the caller's, a
/// `404` when it is not registered, and a `500` when the write fails.
pub async fn heartbeat(
    context: web::Data<AppContext>,
    request: HttpRequest,
    name: web::Path<String>,
    body: web::Bytes,
) -> ApiResult {
    let (caller, row) = owned(&context, &request, &name).await?;
    let beat: Heartbeat = parse(&body, "a heartbeat")?;

    let Some(status) = health::record(context.get_ref(), &row, &beat)
        .await
        .map_err(|err| ApiError::from_human(&err))?
    else {
        return Err(ApiError::not_found(
            "That service is no longer registered. Register it again.",
        ));
    };

    debug!(service = %row.name, actor = %caller.username(), "Recorded a heartbeat.");

    Ok(json_ok(&status))
}

/// `DELETE /api/v1/services/{name}`.
///
/// # Errors
///
/// A `401` without a credential, a `403` when the service is not the caller's, a
/// `404` when it is not registered, and a `500` when the write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    request: HttpRequest,
    name: web::Path<String>,
) -> ApiResult {
    let (caller, row) = owned(&context, &request, &name).await?;

    registry::remove(context.get_ref(), &row.name, caller.username())
        .await
        .map_err(|err| failed(&context, err))?;

    Ok(HttpResponse::NoContent().finish())
}

/// `GET /api/v1/services/{name}/config`.
///
/// # Errors
///
/// A `401` without a credential, a `403` when the service is not the caller's, a
/// `404` when it is not registered, and a `500` when the read fails.
pub async fn get_config(
    context: web::Data<AppContext>,
    request: HttpRequest,
    name: web::Path<String>,
) -> ApiResult {
    let (_, row) = owned(&context, &request, &name).await?;

    Ok(json_ok(&row.config))
}

/// `PUT /api/v1/services/{name}/config`.
///
/// Administrators only, even though the service itself may read it: a plugin
/// that could rewrite its own configuration would make the admin UI's copy a
/// suggestion rather than a setting.
///
/// # Errors
///
/// A `400` for a body that is not a JSON object, a `401` without a credential, a
/// `403` for anybody but an administrator, a `404` when it is not registered, and
/// a `500` when the write fails.
pub async fn put_config(
    context: web::Data<AppContext>,
    request: HttpRequest,
    name: web::Path<String>,
    body: web::Bytes,
) -> ApiResult {
    let caller = caller(&context, &request).await?;
    caller.require_admin().map_err(refusal)?;

    let config: serde_json::Value = parse(&body, "a configuration document")?;

    if !config.is_object() {
        return Err(ApiError::bad_request(
            "A service's configuration has to be a JSON object.",
        ));
    }

    let row = registry::configure(
        context.get_ref(),
        &service_name(&name)?,
        config,
        caller.username(),
    )
    .await
    .map_err(|err| failed(&context, err))?;

    Ok(json_ok(&row.config))
}

/// Resolves the caller and the named service, refusing one that is not theirs.
async fn owned(
    context: &AppContext,
    request: &HttpRequest,
    name: &str,
) -> Result<(Caller, ServiceRow), ApiError> {
    let caller = caller(context, request).await?;
    let row = registry::require(context, &service_name(name)?)
        .await
        .map_err(|err| failed(context, err))?;

    caller.require_owns(&row).map_err(refusal)?;

    Ok((caller, row))
}

/// Who this request is from, as a handler wants it.
async fn caller(context: &AppContext, request: &HttpRequest) -> Result<Caller, ApiError> {
    auth::caller(context, request).await.map_err(refusal)
}

/// Parses a request body, after the caller has been authenticated.
///
/// Deliberately not a `web::Json<T>` extractor: an extractor runs *before* the
/// handler body, so a malformed body from a caller with no credential would be
/// answered `400` and tell them their request reached a route that exists. Every
/// one of these endpoints answers `401` first.
fn parse<T: DeserializeOwned>(body: &web::Bytes, what: &str) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|err| ApiError::bad_request(format!("That is not {what} we can read: {err}.")))
}

/// A service name from the path, refusing one that could never name a service.
fn service_name(raw: &str) -> Result<ServiceName, ApiError> {
    ServiceName::parse(raw).map_err(|err| ApiError::bad_request(err.to_string()))
}

/// What each way of being refused looks like on the wire.
///
/// One message for `Rejected` whatever caused it: telling a caller that their
/// token was merely expired, or that no such account exists, is an oracle.
///
/// Shared with [`super::events`], whose feed is authenticated the same way.
pub(super) fn refusal(failure: AuthFailure) -> ApiError {
    match failure {
        AuthFailure::Rejected => ApiError::unauthorized(
            "That credential is not one this server accepts for the control API.",
        ),
        AuthFailure::Forbidden(message) => ApiError::forbidden(message),
        AuthFailure::RateLimited(retry_after) => ApiError::new(
            actix_web::http::StatusCode::TOO_MANY_REQUESTS,
            format!(
                "Too many attempts. Try again in {} minutes.",
                retry_after.num_minutes().max(1)
            ),
        ),
        AuthFailure::Unavailable(err) => {
            error!(error = %err, "Could not resolve who a control-API request is from.");

            ApiError::internal()
        }
    }
}

/// What each registry failure looks like on the wire.
fn failed(context: &AppContext, err: RegistryError) -> ApiError {
    match err {
        RegistryError::Taken(name) => ApiError::conflict(format!(
            "The service name '{name}' is registered to a different account."
        ))
        .with_code("service_name_taken"),
        RegistryError::Unknown(name) => {
            ApiError::not_found(format!("No service named '{name}' is registered."))
        }
        RegistryError::Unavailable(err) => {
            context.session().record_human_error(&err);

            ApiError::from_human(&err)
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{CredentialKind, ServiceState, ServiceSummary};

    use super::*;
    use crate::db::repos::{NewUser, UserRow};
    use crate::identity::credentials::{MintRequest, mint};
    use crate::testing::TestServer;

    /// A service account, its token, and a registration under `name`.
    async fn sidecar(server: &TestServer, name: &str) -> (UserRow, String) {
        let username = Username::parse(&format!("svc.{name}")).unwrap();
        let user = server
            .db()
            .users()
            .create(NewUser::service(username.clone()))
            .await
            .unwrap();
        let minted = mint(
            server.db(),
            &server.config().auth,
            &user,
            MintRequest::new(CredentialKind::ServiceToken, "Test sidecar", &username),
        )
        .await
        .unwrap();

        (user, minted.secret.expose().to_string())
    }

    fn descriptor(name: &str) -> serde_json::Value {
        serde_json::json!({ "name": name, "version": "1.2.3" })
    }

    macro_rules! app {
        ($server:expr) => {
            test::init_service(App::new().configure($server.app())).await
        };
    }

    #[actix_web::test]
    async fn a_service_registers_heartbeats_and_reads_its_own_configuration() {
        let server = TestServer::start().await;
        let (_, token) = sidecar(&server, "weather").await;
        let app = app!(server);

        let registered = test::TestRequest::post()
            .uri("/api/v1/services/register")
            .insert_header(("authorization", format!("Bearer {token}")))
            .set_json(descriptor("weather"))
            .send_request(&app)
            .await;
        assert_eq!(registered.status(), StatusCode::OK);
        let summary: ServiceSummary = test::read_body_json(registered).await;
        assert_eq!(summary.descriptor.name.as_str(), "weather");
        assert_eq!(summary.status.state, ServiceState::Unknown);

        let beat = test::TestRequest::post()
            .uri("/api/v1/services/weather/heartbeat")
            .insert_header(("authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "state": "healthy", "metrics": { "seen": 3 } }))
            .send_request(&app)
            .await;
        assert_eq!(beat.status(), StatusCode::OK);

        let config = test::TestRequest::get()
            .uri("/api/v1/services/weather/config")
            .insert_header(("authorization", format!("Bearer {token}")))
            .send_request(&app)
            .await;
        assert_eq!(config.status(), StatusCode::OK);
        assert_eq!(
            test::read_body_json::<serde_json::Value, _>(config).await,
            serde_json::json!({})
        );
    }

    #[actix_web::test]
    async fn only_an_administrator_may_write_a_services_configuration() {
        // The rule the brief names: a service reads its own configuration and an
        // administrator writes it. A plugin that could write its own would make
        // the admin UI's copy a suggestion.
        let server = TestServer::start().await;
        let (_, token) = sidecar(&server, "weather").await;
        let app = app!(server);
        test::TestRequest::post()
            .uri("/api/v1/services/register")
            .insert_header(("authorization", format!("Bearer {token}")))
            .set_json(descriptor("weather"))
            .send_request(&app)
            .await;

        let refused = test::TestRequest::put()
            .uri("/api/v1/services/weather/config")
            .insert_header(("authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "interval": 60 }))
            .send_request(&app)
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        let (_, session) = server.signed_in("ada", true).await;
        let written = test::TestRequest::put()
            .uri("/api/v1/services/weather/config")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .set_json(serde_json::json!({ "interval": 60 }))
            .send_request(&app)
            .await;
        assert_eq!(written.status(), StatusCode::OK);

        // …and the service reads back what was written.
        let read = test::TestRequest::get()
            .uri("/api/v1/services/weather/config")
            .insert_header(("authorization", format!("Bearer {token}")))
            .send_request(&app)
            .await;
        assert_eq!(
            test::read_body_json::<serde_json::Value, _>(read).await,
            serde_json::json!({ "interval": 60 })
        );
    }

    #[actix_web::test]
    async fn one_service_may_not_read_or_change_another() {
        let server = TestServer::start().await;
        let (_, weather) = sidecar(&server, "weather").await;
        let (_, adsb) = sidecar(&server, "adsb").await;
        let app = app!(server);

        for (token, name) in [(&weather, "weather"), (&adsb, "adsb")] {
            test::TestRequest::post()
                .uri("/api/v1/services/register")
                .insert_header(("authorization", format!("Bearer {token}")))
                .set_json(descriptor(name))
                .send_request(&app)
                .await;
        }

        for (method, uri) in [
            ("GET", "/api/v1/services/adsb/config"),
            ("DELETE", "/api/v1/services/adsb"),
        ] {
            let refused = test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .insert_header(("authorization", format!("Bearer {weather}")))
                .send_request(&app)
                .await;

            assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }

        // And registering under the other one's name is a conflict, not a theft.
        let stolen = test::TestRequest::post()
            .uri("/api/v1/services/register")
            .insert_header(("authorization", format!("Bearer {weather}")))
            .set_json(descriptor("adsb"))
            .send_request(&app)
            .await;
        assert_eq!(stolen.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn nothing_here_answers_without_a_credential() {
        let server = TestServer::start().await;
        let app = app!(server);

        for (method, uri) in [
            ("GET", "/api/v1/services"),
            ("POST", "/api/v1/services/register"),
            ("DELETE", "/api/v1/services/weather"),
            ("POST", "/api/v1/services/weather/heartbeat"),
            ("GET", "/api/v1/services/weather/config"),
            ("PUT", "/api/v1/services/weather/config"),
        ] {
            let refused = test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .set_json(serde_json::json!({}))
                .send_request(&app)
                .await;

            assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[actix_web::test]
    async fn an_administrator_lists_and_removes_what_is_registered() {
        let server = TestServer::start().await;
        let (_, token) = sidecar(&server, "weather").await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = app!(server);
        test::TestRequest::post()
            .uri("/api/v1/services/register")
            .insert_header(("authorization", format!("Bearer {token}")))
            .set_json(descriptor("weather"))
            .send_request(&app)
            .await;

        let listed = test::TestRequest::get()
            .uri("/api/v1/services")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let services: Vec<ServiceSummary> = test::read_body_json(listed).await;
        assert_eq!(services.len(), 1);

        let removed = test::TestRequest::delete()
            .uri("/api/v1/services/weather")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);

        // A heartbeat afterwards says "register again" rather than failing.
        let orphaned = test::TestRequest::post()
            .uri("/api/v1/services/weather/heartbeat")
            .insert_header(("authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "state": "healthy" }))
            .send_request(&app)
            .await;
        assert_eq!(orphaned.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_person_cannot_register_a_service_as_themselves() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = app!(server);

        let refused = test::TestRequest::post()
            .uri("/api/v1/services/register")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .set_json(descriptor("weather"))
            .send_request(&app)
            .await;

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }
}
