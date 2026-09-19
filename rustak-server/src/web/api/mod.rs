//! `/api/v1`: the JSON API the admin UI is written against.
//!
//! # Two scopes, and what divides them
//!
//! The public scope holds the things somebody with no session has to be able to
//! reach: the health check, what sign-in methods this installation offers, the
//! sign-in ceremonies themselves, and the first-run wizard. Everything else
//! sits behind [`middleware::api_auth`].
//!
//! "Public" means *outside that gate*, not unauthenticated. `/services/*` and
//! `/events` are mounted there as well, because a sidecar authenticates with a
//! service token or a client certificate and the gate takes one of our own
//! access tokens and nothing else. Those handlers resolve their caller
//! themselves through [`crate::plugins::auth`], and the route-table test below
//! asserts that none of them answers without a credential.
//!
//! The passkey ceremonies are public even for a registration by somebody who is
//! signed in, because the same endpoint also serves the first administrator,
//! who by definition is not. The handler resolves the bearer token itself and
//! refuses a registration that carries neither a session nor a registration
//! token — the check is in the handler rather than the scope because there are
//! two ways to pass it.
//!
//! # No cookies, so no cross-site request forgery
//!
//! The credential is a header the browser attaches only when our own code asks
//! it to. A cross-site page can cause a request to these routes but cannot make
//! the browser authenticate it, so there is no token to double-submit.

pub mod audit;
pub mod auth;
pub mod certificates;
pub mod clients;
pub mod config_packages;
pub mod cot;
pub mod credentials;
pub mod devices;
pub mod error;
pub mod events;
pub mod extract;
pub mod groups;
pub mod health;
pub mod me;
pub mod middleware;
pub mod missions;
pub mod missions_view;
pub mod packages;
pub mod packages_upload;
pub mod passkey;
pub mod profile_files;
pub mod profiles;
pub mod services;
pub mod settings;
pub mod setup;
pub mod subject;
pub mod users;
pub mod users_groups;

use actix_web::body::BoxBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::from_fn;
use actix_web::web;

pub use error::{ApiError, ApiResult, json_error, json_ok};
pub use extract::{Administrative, Authenticated, Identity};

/// The version prefix every route here sits under.
pub const API_ROOT: &str = "/api/v1";

/// The largest JSON body any `/api/v1` route will read.
///
/// No `JsonConfig` was installed anywhere in the crate, so actix's 2 MiB
/// default applied to the attacker-supplied `credential` blob that
/// `finish_registration` base64-decodes and CBOR-parses (R-01 L16) and to the
/// unbounded `message` a service heartbeat carries (R-01 M12). Nothing this API
/// accepts as JSON is remotely this large — the biggest is a device profile —
/// and bulk content arrives as multipart on the Marti surface instead.
const JSON_LIMIT: usize = 256 * 1024;

/// Registers every `/api/v1` route.
pub fn configure() -> actix_web::Scope<
    impl actix_web::dev::ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = ServiceResponse<BoxBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    public(web::scope(API_ROOT).app_data(web::JsonConfig::default().limit(JSON_LIMIT))).service(
        web::scope("")
            .wrap(from_fn(middleware::api_auth))
            .route("/me", web::get().to(me::me))
            .route("/me/oidc-link", web::post().to(me::link_oidc))
            .route("/auth/logout", web::post().to(auth::logout))
            .route("/auth/passkeys", web::get().to(passkey::list))
            .route("/auth/passkeys/{id}", web::delete().to(passkey::remove))
            .route("/users", web::get().to(users::list))
            .route("/users", web::post().to(users::create))
            .route("/users/{username}", web::get().to(users::get))
            .route("/users/{username}", web::patch().to(users::patch))
            .route("/users/{username}/groups", web::get().to(users_groups::get))
            .route("/users/{username}/groups", web::put().to(users_groups::put))
            .route("/groups", web::get().to(groups::list))
            .route("/groups", web::post().to(groups::create))
            .route("/groups/{name}", web::patch().to(groups::patch))
            .route("/groups/{name}", web::delete().to(groups::remove))
            .route("/groups/{name}/members", web::get().to(groups::members))
            .route("/credentials", web::get().to(credentials::list))
            .route("/credentials", web::post().to(credentials::create))
            .route("/credentials/{id}", web::delete().to(credentials::remove))
            .route(
                "/credentials/{id}/enroll-url",
                web::get().to(credentials::enroll_template),
            )
            .route("/devices", web::get().to(devices::list))
            .route("/devices/{uid}", web::get().to(devices::get))
            .route("/devices/{uid}", web::delete().to(devices::remove))
            .route(
                "/devices/{uid}/active-groups",
                web::get().to(devices::active_groups),
            )
            .route(
                "/devices/{uid}/active-groups",
                web::put().to(devices::set_active_groups),
            )
            .route("/certificates", web::get().to(certificates::list))
            .route("/certificates/{id}", web::get().to(certificates::get))
            .route(
                "/certificates/{id}/revoke",
                web::post().to(certificates::revoke),
            )
            .route("/audit", web::get().to(audit::list))
            .route("/settings", web::get().to(settings::get))
            .route("/settings/files", web::get().to(settings::files))
            .route("/settings/files", web::put().to(settings::put_files))
            .route("/settings/marti", web::get().to(settings::marti))
            .route("/settings/tls", web::get().to(settings::tls))
            .route("/settings/tls/renew", web::post().to(settings::renew_tls))
            .route("/setup/server", web::post().to(setup::server))
            .route("/setup/ca", web::get().to(setup::get_ca))
            .route("/setup/ca", web::post().to(setup::ca))
            .route("/setup/complete", web::post().to(setup::complete))
            // Device profiles and the manual configuration package, whose own
            // registration order puts `/profiles/pref-catalog` ahead of the
            // `{id}` that would otherwise swallow it.
            .configure(missions::routes)
            .configure(profiles::routes)
            .configure(config_packages::routes)
            // Stored files, live connections and relayed CoT, each registering
            // its literal segments ahead of the `{hash}`/`{uid}` that would
            // otherwise swallow them.
            .configure(packages::routes)
            .configure(clients::routes)
            .configure(cot::routes),
    )
}

/// Registers the routes somebody with no session has to be able to reach.
///
/// Mounted directly on the versioned scope rather than in a nested one,
/// because a nested `web::scope("")` matches every path under it and nothing
/// would ever reach the guarded scope that follows. Actix matches services in
/// registration order, so these answer first and everything else falls through
/// to the gate.
fn public<T>(scope: actix_web::Scope<T>) -> actix_web::Scope<T>
where
    T: actix_web::dev::ServiceFactory<
            ServiceRequest,
            Config = (),
            Response = ServiceResponse<BoxBody>,
            Error = actix_web::Error,
            InitError = (),
        >,
{
    scope
        .route("/health", web::get().to(health::health))
        .route("/auth/metadata", web::get().to(auth::metadata))
        .route("/auth/token", web::post().to(auth::token))
        .route("/auth/refresh", web::post().to(auth::refresh))
        .route(
            "/auth/passkey/register/start",
            web::post().to(passkey::register_start),
        )
        .route(
            "/auth/passkey/register/finish",
            web::post().to(passkey::register_finish),
        )
        .route(
            "/auth/passkey/login/start",
            web::post().to(passkey::login_start),
        )
        .route(
            "/auth/passkey/login/finish",
            web::post().to(passkey::login_finish),
        )
        .route("/setup/status", web::get().to(setup::status))
        .route("/setup/admin", web::post().to(setup::admin))
        // The control API and the server-event feed: mounted here because they
        // accept a service token or a client certificate, which the session gate
        // below would refuse. Every one of their handlers resolves its own
        // caller through `plugins::auth` and answers `401` without one — see
        // `services::routes`.
        .configure(services::routes)
        .configure(events::routes)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};

    use crate::prelude::*;

    use super::*;

    /// Every route that must not be reachable without a session.
    const PROTECTED: &[(&str, &str)] = &[
        ("GET", "/api/v1/me"),
        ("POST", "/api/v1/me/oidc-link"),
        ("POST", "/api/v1/auth/logout"),
        ("GET", "/api/v1/auth/passkeys"),
        ("DELETE", "/api/v1/auth/passkeys/1"),
        ("GET", "/api/v1/users"),
        ("POST", "/api/v1/users"),
        ("GET", "/api/v1/users/ada"),
        ("PATCH", "/api/v1/users/ada"),
        ("GET", "/api/v1/users/ada/groups"),
        ("PUT", "/api/v1/users/ada/groups"),
        ("GET", "/api/v1/groups"),
        ("POST", "/api/v1/groups"),
        ("PATCH", "/api/v1/groups/Blue"),
        ("DELETE", "/api/v1/groups/Blue"),
        ("GET", "/api/v1/groups/Blue/members"),
        ("GET", "/api/v1/credentials"),
        ("POST", "/api/v1/credentials"),
        ("DELETE", "/api/v1/credentials/1"),
        ("GET", "/api/v1/credentials/1/enroll-url"),
        ("GET", "/api/v1/devices"),
        ("GET", "/api/v1/devices/ANDROID-1"),
        ("DELETE", "/api/v1/devices/ANDROID-1"),
        ("GET", "/api/v1/devices/ANDROID-1/active-groups"),
        ("PUT", "/api/v1/devices/ANDROID-1/active-groups"),
        ("GET", "/api/v1/certificates"),
        ("GET", "/api/v1/certificates/1"),
        ("POST", "/api/v1/certificates/1/revoke"),
        ("GET", "/api/v1/audit"),
        ("GET", "/api/v1/settings"),
        ("GET", "/api/v1/settings/tls"),
        ("POST", "/api/v1/settings/tls/renew"),
        ("GET", "/api/v1/settings/files"),
        ("PUT", "/api/v1/settings/files"),
        ("GET", "/api/v1/settings/marti"),
        ("GET", "/api/v1/packages"),
        ("POST", "/api/v1/packages"),
        ("GET", "/api/v1/packages/aa"),
        ("PATCH", "/api/v1/packages/aa"),
        ("DELETE", "/api/v1/packages/aa"),
        ("GET", "/api/v1/packages/aa/content"),
        ("GET", "/api/v1/clients"),
        ("GET", "/api/v1/clients/history"),
        ("GET", "/api/v1/clients/status"),
        ("DELETE", "/api/v1/clients/ANDROID-1"),
        ("POST", "/api/v1/clients/ANDROID-1/incognito"),
        ("GET", "/api/v1/cot"),
        ("GET", "/api/v1/cot/ANDROID-1"),
        ("DELETE", "/api/v1/cot/ANDROID-1"),
        ("GET", "/api/v1/cot/ANDROID-1/history"),
        ("POST", "/api/v1/setup/server"),
        ("GET", "/api/v1/setup/ca"),
        ("POST", "/api/v1/setup/ca"),
        ("POST", "/api/v1/setup/complete"),
        // Outside the gate, but not open: each of these resolves its own caller.
        ("GET", "/api/v1/services"),
        ("POST", "/api/v1/services/register"),
        ("DELETE", "/api/v1/services/weather"),
        ("POST", "/api/v1/services/weather/heartbeat"),
        ("GET", "/api/v1/services/weather/config"),
        ("PUT", "/api/v1/services/weather/config"),
        ("GET", "/api/v1/events"),
    ];

    #[actix_web::test]
    async fn nothing_behind_the_gate_answers_without_a_session() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(context))
                .service(configure()),
        )
        .await;

        for (method, uri) in PROTECTED {
            let request = test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .set_json(serde_json::json!({}))
                .to_request();

            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} answered without a session",
            );
        }
    }

    #[actix_web::test]
    async fn the_public_routes_are_reachable_without_one() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(context))
                .service(configure()),
        )
        .await;

        for (method, uri) in [
            ("GET", "/api/v1/health"),
            ("GET", "/api/v1/auth/metadata"),
            ("GET", "/api/v1/setup/status"),
        ] {
            let request = test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .to_request();

            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::OK,
                "{method} {uri} was not reachable",
            );
        }
    }
}
