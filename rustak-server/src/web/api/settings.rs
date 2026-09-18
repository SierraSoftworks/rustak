//! `/api/v1/settings`: what this server calls itself, and the subsystem
//! settings an operator adjusts or is shown.
//!
//! Administrative throughout, because the host names and the node identifier
//! are what an enrolment package is built from, and an installation should not
//! have to publish them to everybody who can sign in.
//!
//! # The configuration file wins, everywhere
//!
//! A value an operator wrote into a file they deploy is not something a web
//! form gets to replace, so every area here reports whether the file decided
//! it — and the one writable value refuses a change that the next start would
//! override, rather than storing a number nothing will read.

use actix_web::http::StatusCode;
use actix_web::web;
use rustak_api::{FileSettings, MartiSettings};

use crate::files::limits;
use crate::identity::settings;
use crate::jobs::{ACME_RENEW_FORCED_KEY, AcmeRenewJob, AcmeRenewTask};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;

/// Answers with the resolved settings: the configuration file's values where it
/// gives any, the wizard's where it does not.
///
/// # Errors
///
/// A `500` when the stored settings cannot be read.
pub async fn get(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let resolved = settings::resolve(&context.config(), context.db())
        .await
        .map_err(|err| {
            context.session().record_human_error(&err);
            ApiError::from_human(&err)
        })?;

    Ok(json_ok(&resolved))
}

/// `GET /api/v1/settings/files` — the upload ceiling and where it came from.
///
/// # Errors
///
/// A `500` when the stored row cannot be read.
pub async fn files(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let resolved = limits::resolve(&context.config(), context.db())
        .await
        .map_err(|err| human(&context, &err))?;

    Ok(json_ok(&resolved))
}

/// `PUT /api/v1/settings/files` — change the upload ceiling.
///
/// Refused with a `409` when the configuration file pins the value: storing a
/// number the next start would override is worse than saying so, because the
/// operator would believe the change had taken.
///
/// # Errors
///
/// A `400` for a limit of zero or an absurd one, a `409` as above, and a `500`
/// when the write fails.
pub async fn put_files(
    context: web::Data<AppContext>,
    body: web::Json<FileSettings>,
    caller: Administrative,
) -> ApiResult {
    if limits::is_pinned(&context.config()) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This server's upload limit is set in its configuration file, \
             so it cannot be changed here.",
        ));
    }

    let saved = limits::save(
        context.db(),
        body.upload_size_limit_mb,
        Some(&caller.user.username),
    )
    .await
    .map_err(|err| human(&context, &err))?;

    Ok(json_ok(&saved))
}

/// `GET /api/v1/settings/marti` — what the TAK surface tells clients.
///
/// Read-only. Both values are decisions for the file an operator deploys: the
/// public host is what makes a download URL resolve on a *peer's* network, and
/// the origin policy decides whether every Marti response is readable by any
/// page an operator's users happen to visit.
///
/// # Errors
///
/// Never.
pub async fn marti(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let config = context.config();

    Ok(json_ok(&MartiSettings {
        public_host: config.marti.public_host.clone(),
        allow_all_origins: config.marti.allow_all_origins,
    }))
}

/// `GET /api/v1/settings/tls` — what the public listener presents.
///
/// Where it came from, when it expires, when it will be renewed, and — for an
/// ACME installation — what the last order said if it failed.
///
/// # Errors
///
/// A `500` when the stored certificate row cannot be read.
pub async fn tls(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let reported = crate::pki::acme::status(context.get_ref())
        .await
        .map_err(|err| human(&context, &err))?;

    Ok(json_ok(&reported))
}

/// `POST /api/v1/settings/tls/renew` — order a certificate now.
///
/// Queued rather than run inline: an order waits for a certificate authority
/// to reach this server and validate a name, which is seconds at best and a
/// timeout at worst, and neither belongs on the end of an admin click. The
/// answer is `202` with the status as it stands; the caller polls
/// [`tls`] to watch it change.
///
/// Repeated calls collapse onto one queued order — every one of them spends
/// real rate limit against the authority.
///
/// # Errors
///
/// A `409` when this installation is not using ACME, and a `500` when the
/// order cannot be queued.
pub async fn renew_tls(context: web::Data<AppContext>, caller: Administrative) -> ApiResult {
    if !context.config().acme.enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This server does not obtain its certificate from an ACME authority, \
             so there is nothing to renew.",
        ));
    }

    AcmeRenewJob::dispatch(
        AcmeRenewTask { forced: true },
        Some(ACME_RENEW_FORCED_KEY.into()),
        context.get_ref(),
    )
    .await
    .map_err(|err| human(&context, &err))?;

    info!(
        actor = %caller.user.username,
        "An administrator asked for the public certificate to be renewed now."
    );

    let reported = crate::pki::acme::status(context.get_ref())
        .await
        .map_err(|err| human(&context, &err))?;

    Ok(actix_web::HttpResponse::Accepted().json(reported))
}

/// A failure of ours, logged in full and generalised for the caller.
fn human(context: &AppContext, err: &Error) -> ApiError {
    context.session().record_human_error(err);

    ApiError::from_human(err)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{ServerSettings, ServerSettingsRequest, TlsSource, TlsStatus};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn an_administrator_is_told_what_the_server_is() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        settings::save(
            server.db(),
            &ServerSettingsRequest {
                name: "Hilltop TAK".to_string(),
                domains: vec!["tak.example.com".to_string()],
                base_url: None,
            },
            None,
        )
        .await
        .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let settings: ServerSettings = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/settings")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            settings.domains,
            vec!["localhost".to_string()],
            "the configuration file's host name wins over the wizard's",
        );
        assert_eq!(settings.name, "Hilltop TAK");
        assert!(settings.node_id.is_some());
    }

    #[actix_web::test]
    async fn an_administrator_is_told_what_the_public_listener_presents() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let status: TlsStatus = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/settings/tls")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        // The test server serves plaintext, and saying so is the point: an
        // administrator asking what is presented gets an answer either way.
        assert_eq!(status.source, TlsSource::None);
        assert!(status.directory.is_none());
        assert!(!status.needs_attention());
    }

    #[actix_web::test]
    async fn asking_a_non_acme_installation_to_renew_says_why_it_cannot() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/settings/tls/renew")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn a_renewal_an_administrator_asked_for_is_queued_rather_than_awaited() {
        let server = TestServer::start_with(|config| {
            config.acme.enabled = true;
            config.acme.accept_tos = true;
            config.acme.domains = vec!["tak.example.com".to_string()];
        })
        .await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/settings/tls/renew")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "an order takes as long as an authority takes; the caller polls instead",
        );

        let queued = server
            .queue()
            .peek::<_, crate::jobs::AcmeRenewTask>(crate::jobs::ACME_RENEW_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(queued.len(), 1);
        assert!(
            queued[0].payload.forced,
            "an order somebody asked for skips the schedule"
        );
    }

    #[actix_web::test]
    async fn somebody_who_is_not_an_administrator_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/settings")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
