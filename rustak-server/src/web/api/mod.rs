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
//!
//! # A path no route here claims is a JSON `404`, never the shell
//!
//! [`crate::web::server::services`] answers everything it does not recognise
//! with the admin UI's single-page shell, which is right for a deep link and
//! wrong for an API: a caller that asked for `/api/v1/…` and was handed
//! `200 text/html` parses HTML as JSON, exactly as
//! [`crate::web::server::marti_services`] already refuses to let a TAK client
//! do. So the guarded scope carries a default service of its own and
//! `unmatched` answers in the same `{"error": …}` shape every other failure
//! on this surface uses.
//!
//! Two consequences worth stating, because the interop probes depend on both
//! (`interop/shared/src/probe.ts`):
//!
//! * **There is no `405` here, and there cannot be.** [`actix_web::Scope::route`]
//!   hoists a route's method guard onto the resource it builds, so a resource
//!   whose method does not match never matches at all — a known path addressed
//!   with the wrong method is indistinguishable from a path that does not
//!   exist, and both get the same `404`.
//! * **The gate still answers first.** [`middleware::api_auth`] wraps the whole
//!   guarded scope including its default service, so an unmatched path
//!   presented without a credential is a `401` rather than a `404`. That is the
//!   right way round: the API does not tell an unauthenticated caller which of
//!   its paths exist.

pub mod audit;
pub mod auth;
pub mod certificates;
pub mod clients;
pub mod cloudtak_onboarding;
pub mod config_packages;
pub mod cot;
pub mod credentials;
pub mod devices;
pub mod error;
pub mod events;
pub mod extract;
pub mod groups;
pub mod health;
pub mod map;
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
use actix_web::http::StatusCode;
use actix_web::middleware::from_fn;
use actix_web::{HttpResponse, web};

pub use error::{ApiError, ApiResult, json_error, json_ok};
pub use extract::{Administrative, Authenticated, Identity};

/// The version prefix every route here sits under.
pub const API_ROOT: &str = "/api/v1";

/// What a caller is told when no route here claims the path they asked for.
///
/// It names neither the path nor the methods it might answer to: the same
/// sentence for a path that does not exist and for one addressed with the wrong
/// method, so the surface is not a map of itself.
const NO_SUCH_ROUTE: &str = "There is no such endpoint on this server.";

/// Answers a path inside `/api/v1` that no route claims.
///
/// See the module documentation for why this exists rather than the single-page
/// shell, and why it is a `404` rather than a `405`.
async fn unmatched() -> HttpResponse {
    json_error(StatusCode::NOT_FOUND, NO_SUCH_ROUTE)
}

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
            // The CloudTAK hand-over, whose download sits on a literal segment
            // of its own so the `{id}.p12` pattern never competes with a bare
            // `{id}` elsewhere.
            .configure(cloudtak_onboarding::routes)
            // Stored files, live connections and relayed CoT, each registering
            // its literal segments ahead of the `{hash}`/`{uid}` that would
            // otherwise swallow them.
            .configure(packages::routes)
            .configure(clients::routes)
            .configure(cot::routes)
            .configure(map::routes)
            // Last, and reached by every path the routes above did not claim:
            // `web::scope("")` matches any prefix, so this is where `/api/v1`
            // stops rather than falling on to the admin UI's catch-all.
            .default_service(web::to(unmatched)),
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
        ("POST", "/api/v1/users/ada/cloudtak-onboarding"),
        ("GET", "/api/v1/cloudtak-onboarding/abc.p12"),
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

    /// Paths inside `/api/v1` that no route claims.
    ///
    /// The first four are *known* paths addressed with the method they do not
    /// answer to, which is the case with a history: `interop/shared/src/probe.ts`
    /// reads a surface as absent on a `404` or on HTML with a success status, so
    /// probing a POST-only path with a `GET` reads as "not served yet" and
    /// silently skips every scenario for the brief that owns it. That is the
    /// trap `tests/cloudtak_onboarding.rs` documents, and it is why the probes
    /// aim at the download rather than at the preparation.
    const UNMATCHED: &[(&str, &str)] = &[
        ("GET", "/api/v1/users/ada/cloudtak-onboarding"),
        ("GET", "/api/v1/auth/logout"),
        ("POST", "/api/v1/certificates"),
        ("DELETE", "/api/v1/audit"),
        ("GET", "/api/v1/no-such-thing"),
        ("GET", "/api/v1/users/ada/no-such-thing"),
    ];

    #[actix_web::test]
    async fn a_path_no_route_claims_is_a_json_not_found_rather_than_the_shell() {
        // The whole application, not `configure()` alone: what is being proved
        // is that `/api/v1` stops here rather than falling on to the single-page
        // shell that `web::server::services` mounts behind it, and only the
        // assembled routes can show that.
        let server = crate::testing::TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (_, session) = server.signed_in("ada", true).await;
        let token = crate::testing::context::bearer(&session);

        for (method, uri) in UNMATCHED {
            let request = test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .insert_header(("authorization", token.clone()))
                .to_request();
            let response = test::call_service(&app, request).await;
            let status = response.status();
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            assert_eq!(
                content_type.as_deref(),
                Some("application/json"),
                "{method} {uri} answered with something a JSON client cannot read",
            );
        }
    }

    #[actix_web::test]
    async fn an_unmatched_path_is_refused_before_it_is_looked_for() {
        // `api_auth` wraps the guarded scope's default service too, so a caller
        // with no credential cannot use the difference between `401` and `404`
        // to learn which paths this surface has.
        let server = crate::testing::TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for (method, uri) in UNMATCHED {
            let request = test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .to_request();

            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} told an anonymous caller whether it exists",
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
