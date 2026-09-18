//! The TAK-compatible HTTP surface: `/Marti/**` and `/files/api/config`.
//!
//! # What is here in M2-04
//!
//! The foundations every later Marti brief is written against — the envelope
//! and its `type` strings ([`response`]), the refusal shape ([`error`]), the
//! extractors ([`extract`], [`principal`]), the transport headers
//! ([`headers`]), the date formats ([`time`]) — plus the version, identity and
//! stub endpoints that a client probes before it does anything else
//! ([`version`], [`util`], [`stubs`]). Groups, contacts, missions, Enterprise
//! Sync and device profiles arrive in M2-08, M3 and M4 and mount into the same
//! scope.
//!
//! # Why the whole scope is mounted on both listeners
//!
//! A real TAK Server serves the full API on its mutually authenticated port
//! (`:8443`) and again on the browser-facing one (`:8446`), and CloudTAK is
//! configured with three independent URLs that may all point at the same host.
//! The routes are therefore identical; [`auth_policy`] is the only thing that
//! differs per listener, and today it says the same for both because bearer
//! resolution is all M2 has so far. See [`principal`] for that seam.
//!
//! # Registration order
//!
//! actix matches services in registration order, so a literal segment has to be
//! registered before a `{name}` that would also match it. The mission scope
//! (M4) will have most of those; what is here keeps the same discipline —
//! `/Marti/api/version/config` and `/Marti/api/version/info` are registered
//! before `/Marti/api/version`, and `/Marti/api/missions/{name}/kml` is
//! registered here so that M4's mission scope inherits the literal rather than
//! having to re-add it.
//!
//! # Nothing in here may answer with a `3xx`
//!
//! node-tak reads every status below 400 as success and parses the redirect's
//! empty body as the payload, so a redirect is a client failure that looks like
//! a server success. Three things enforce it: `NormalizePath` is never
//! installed, the scope's [`default_service`](actix_web::Scope::default_service)
//! turns an unmatched path or method into a JSON `404`/`405` rather than a
//! canonicalising redirect, and [`headers::marti_headers`] replaces any `3xx`
//! that reaches it with a `500` and a log line.

pub mod channels;
pub mod contacts;
pub mod cot;
pub mod enroll;
pub mod error;
pub mod extract;
pub mod files;
pub mod groups;
pub mod headers;
pub mod login;
pub mod missions;
pub mod oauth;
pub mod principal;
pub mod profiles;
pub mod response;
pub mod stubs;
pub mod subscriptions;
pub mod sync;
pub mod sync_metadata;
pub mod sync_read;
pub mod time;
pub mod tls;
pub mod util;
pub mod version;

use actix_web::middleware::from_fn;
use actix_web::{HttpRequest, web};

pub use error::{MartiError, MartiResult};
pub use extract::{ApiVersion, CiQuery, CommaList, ListenerRole, LooseBool, MissionRef};
pub use principal::{AuthPolicy, MartiPrincipal, auth_policy};
pub use response::{ApiResponse, kind};

/// The prefix every TAK route but one sits under.
pub const MARTI_ROOT: &str = "/Marti";

/// The one that does not: CloudTAK's setup gate lives at the site root.
pub const FILES_ROOT: &str = "/files/api";

/// The OAuth2 endpoints, which are not under `/Marti` either.
///
/// A third scope rather than routes on the application, so that they inherit
/// the same transport headers and the same never-redirect guard: a `3xx` from
/// `/oauth/token` is read by node-tak as a success carrying an empty body.
pub const OAUTH_ROOT: &str = "/oauth";

/// Registers the whole Marti surface for one listener.
///
/// Two scopes rather than one, because `/files/api/config` has no `/Marti`
/// prefix — it is the path CloudTAK's own setup call uses, and moving it would
/// mean a CloudTAK that cannot save a connection.
///
/// Each scope carries its own default service so that an unmatched path or an
/// unsupported method is answered here, in the Marti error shape, rather than
/// falling through to the single-page-application catch-all that would answer a
/// missing `/Marti` route with HTML and a `200`.
pub fn services(role: ListenerRole) -> impl FnOnce(&mut web::ServiceConfig) + Clone {
    move |config| {
        config
            .app_data(web::Data::new(role))
            // Before every scope below: the sign-in flow redirects, so it may
            // not sit inside a scope whose middleware turns a `3xx` into a
            // `500`, and `/oauth/authorize` has to be matched before the
            // `/oauth` scope claims the prefix. See `login` for both.
            .configure(login::routes(role))
            .service(
                web::scope(MARTI_ROOT)
                    .wrap(from_fn(headers::marti_headers))
                    .configure(api_routes)
                    .configure(servlet_routes)
                    .configure(sync::routes)
                    .default_service(web::to(unmatched)),
            )
            .service(
                web::scope(FILES_ROOT)
                    .wrap(from_fn(headers::marti_headers))
                    .route("/config", web::get().to(version::files_config))
                    .default_service(web::to(unmatched)),
            )
            .service(
                web::scope(OAUTH_ROOT)
                    .wrap(from_fn(headers::marti_headers))
                    .route("/token", web::post().to(oauth::token))
                    .route("/token_key", web::get().to(oauth::token_key))
                    .route("/jwks", web::get().to(oauth::jwks))
                    .default_service(web::to(unmatched)),
            );
    }
}

/// Everything under `/Marti/api`.
fn api_routes(config: &mut web::ServiceConfig) {
    config.service(
        web::scope("/api")
            // The two literal children before the bare `/version` that would
            // not match them, but would match a future `/version/{anything}`.
            .route("/version/config", web::get().to(version::version_config))
            .route("/version/info", web::get().to(version::version_info))
            .route("/version", web::get().to(version::version))
            .route("/node/id", web::get().to(version::node_id))
            // Enrolment, before the `/util` and stub routes so that the three
            // literal `tls` children are unambiguous whatever is added later.
            .route("/tls/config", web::get().to(tls::config))
            .route("/tls/signClient/v2", web::post().to(tls::sign_client_v2))
            .route("/tls/signClient", web::post().to(tls::sign_client_v1))
            // Device profiles (M3-02), whose own registration order puts the
            // literal `device/profile` children ahead of the `{name}` that
            // would otherwise swallow them.
            .configure(profiles::routes)
            .route("/util/user/roles", web::get().to(util::roles))
            .route("/util/isAdmin", web::get().to(version::is_admin))
            .route("/home", web::get().to(util::home))
            .configure(channel_routes)
            // Enterprise Sync (M3-01): the metadata routes register their two
            // literal children before the `{field}` that would swallow them,
            // and the file manager its `metadata` literals before `{hash}`.
            .configure(sync_metadata::routes)
            .configure(files::routes)
            // The CoT query surface (R-02 H1), before the stubs so that the
            // literal `/cot/**` paths are settled ahead of anything that would
            // match them.
            .configure(cot::routes)
            .configure(stub_routes)
            // Data Sync (M4-01), after the `/missions/{name}/kml` stub above
            // so that the literal keeps winning, and with its own registration
            // order for the literals that would be read as mission names.
            .configure(missions::routes)
            // Its own, rather than the application's: a nested scope inherits
            // the *App*'s default service, which is the single-page shell, and
            // a missing `/Marti/api` route would be answered with HTML and a
            // `200` that node-tak would parse as a payload.
            .default_service(web::to(unmatched)),
    );
}

/// Channels, contacts and subscriptions, all under `/Marti/api`.
///
/// Every literal is registered before the parameterised shape that would also
/// match it: `/groups/all` and the four other one-segment children come first,
/// and `/subscriptions/incognito/{uid}` before `/subscriptions/{uid}/filter`,
/// which would otherwise claim a subscription called `incognito`.
fn channel_routes(config: &mut web::ServiceConfig) {
    config
        .route("/groups/all", web::get().to(groups::all))
        .route("/groups/active", web::put().to(groups::set_active))
        .route("/groups/activebits", web::put().to(groups::set_active_bits))
        .route(
            "/groups/groupCacheEnabled",
            web::get().to(groups::cache_enabled),
        )
        .route("/groups/user", web::get().to(groups::for_user))
        .route("/groups/{name}/{direction}", web::get().to(groups::one))
        .route("/users/all", web::get().to(groups::users_all))
        .route("/contacts/all", web::get().to(contacts::all))
        .route(
            "/clientEndPoints",
            web::get().to(contacts::client_endpoints),
        )
        .route("/subscriptions/all", web::get().to(subscriptions::all))
        .route(
            "/subscriptions/incognito/{uid}",
            web::post().to(subscriptions::incognito),
        )
        .route(
            "/subscriptions/delete/{uid}",
            web::delete().to(subscriptions::delete),
        )
        .route(
            "/subscriptions/{uid}/filter",
            web::put().to(subscriptions::filter),
        )
        .route(
            "/subscriptions/{uid}/filter",
            web::delete().to(subscriptions::filter),
        )
        .route("/subscription/{uid}", web::get().to(subscriptions::one));
}

/// The feature stubs, all under `/Marti/api`.
fn stub_routes(config: &mut web::ServiceConfig) {
    config
        .route("/video", web::get().to(stubs::video))
        .route("/video", web::post().to(stubs::video_write))
        .route("/video/{uid}", web::get().to(stubs::video_feed))
        .route("/video/{uid}", web::put().to(stubs::video_write))
        .route("/video/{uid}", web::delete().to(stubs::video_write))
        .route("/injectors/cot/uid", web::get().to(stubs::injectors))
        .route("/injectors/cot/uid", web::post().to(stubs::injector_write))
        .route("/injectors/cot/uid/{uid}", web::get().to(stubs::injector))
        .route(
            "/injectors/cot/uid/{uid}",
            web::delete().to(stubs::injector_write),
        )
        .route("/repeater/list", web::get().to(stubs::repeater_list))
        .route("/repeater/period", web::get().to(stubs::repeater_period))
        .route("/repeater/period", web::post().to(stubs::repeater_period))
        .route(
            "/repeater/remove/{uid}",
            web::get().to(stubs::repeater_remove),
        )
        // Registered before M4's `/missions/{name}` routes exist, so that the
        // literal `kml` child is already in place when they arrive.
        .route("/missions/{name}/kml", web::get().to(stubs::kml));
}

/// The legacy servlets, which sit directly under `/Marti`.
fn servlet_routes(config: &mut web::ServiceConfig) {
    config
        .route("/GetTime", web::get().to(util::get_time))
        .route("/ErrorLog", web::post().to(util::error_log))
        .route("/vcm", web::get().to(stubs::vcm))
        .route("/vcu", web::get().to(stubs::vcm_write))
        .route("/vcu", web::post().to(stubs::vcm_write))
        .route("/vcs", web::get().to(stubs::vcm_write))
        .route("/vcs", web::post().to(stubs::vcm_write))
        .route("/ExportMissionKML", web::get().to(stubs::kml))
        .route("/KmlMasterSA", web::get().to(stubs::kml))
        .route("/LatestKML", web::get().to(stubs::kml))
        .route("/TracksKML", web::get().to(stubs::kml))
        .route("/sync/missioncreate", web::post().to(stubs::mission_create));
}

/// What a path or method we do not serve is answered with.
///
/// A `405` when the path is one we serve under another method and a `404`
/// otherwise, both as JSON — never a redirect to a spelling that would have
/// matched. A default service cannot ask the router which of the two it is, so
/// the answer comes from [`serves_path`], the one table that lists what is
/// mounted.
async fn unmatched(request: HttpRequest) -> MartiResult {
    if serves_path(request.path()) {
        return Err(MartiError::MethodNotAllowed(request.method().to_string()));
    }

    Err(MartiError::NotFound(request.path().to_string()))
}

/// Every path mounted above, with no method attached.
///
/// Literal rather than reflected over the router, because actix does not expose
/// its route table. A path added without a line here answers `404` where it
/// meant `405`, which the contract test in this module catches.
const PATHS: &[&str] = &[
    "/Marti/api/tls/config",
    "/Marti/api/tls/signClient",
    "/Marti/api/tls/signClient/v2",
    "/Marti/api/tls/profile/enrollment",
    "/Marti/api/device/profile/connection",
    "/oauth/token",
    "/oauth/token_key",
    "/oauth/jwks",
    "/Marti/api/version",
    "/Marti/api/version/config",
    "/Marti/api/version/info",
    "/Marti/api/node/id",
    "/Marti/api/util/user/roles",
    "/Marti/api/util/isAdmin",
    "/Marti/api/home",
    "/Marti/api/groups/all",
    "/Marti/api/groups/active",
    "/Marti/api/groups/activebits",
    "/Marti/api/groups/groupCacheEnabled",
    "/Marti/api/groups/user",
    "/Marti/api/users/all",
    "/Marti/api/contacts/all",
    "/Marti/api/clientEndPoints",
    "/Marti/api/subscriptions/all",
    "/Marti/api/cot",
    "/Marti/api/cot/sa",
    "/Marti/api/cot/matchUid",
    "/Marti/api/missions",
    "/Marti/api/pagedmissions",
    "/Marti/api/missioncount",
    "/Marti/api/video",
    "/Marti/api/injectors/cot/uid",
    "/Marti/api/repeater/list",
    "/Marti/api/repeater/period",
    "/Marti/GetTime",
    "/Marti/ErrorLog",
    "/Marti/vcm",
    "/Marti/vcu",
    "/Marti/vcs",
    "/Marti/ExportMissionKML",
    "/Marti/KmlMasterSA",
    "/Marti/LatestKML",
    "/Marti/TracksKML",
    "/Marti/sync/missioncreate",
    "/Marti/sync/upload",
    "/Marti/sync/search",
    "/Marti/sync/content",
    "/Marti/sync/missionupload",
    "/Marti/sync/missionquery",
    "/Marti/sync/delete",
    "/Marti/api/sync/search",
    "/Marti/api/files/metadata",
    "/Marti/api/files/metadata/count",
    "/files/api/config",
];

/// The paths above that end in one path parameter.
///
/// Matched as "this prefix plus exactly one more non-empty segment", so that
/// `/Marti/api/video/a/b` is a `404` rather than being counted as a feed named
/// `a/b`.
const PARAMETERISED: &[&str] = &[
    "/Marti/api/video/",
    "/Marti/api/cot/xml/",
    "/Marti/api/injectors/cot/uid/",
    "/Marti/api/repeater/remove/",
    "/Marti/api/subscription/",
    "/Marti/api/subscriptions/incognito/",
    "/Marti/api/subscriptions/delete/",
    "/Marti/api/files/",
];

/// The paths that are a prefix, one path parameter, then a literal tail.
const PARAMETERISED_TAIL: &[(&str, &str)] = &[
    ("/Marti/api/missions/", "/kml"),
    ("/Marti/api/cot/xml/", "/all"),
    ("/Marti/api/subscriptions/", "/filter"),
    ("/Marti/api/files/", "/metadata"),
];

/// The paths that are a prefix and exactly two path parameters.
const PARAMETERISED_PAIR: &[&str] = &["/Marti/api/groups/", "/Marti/api/sync/metadata/"];

/// Whether some method serves this exact path.
fn serves_path(path: &str) -> bool {
    if PATHS.contains(&path) {
        return true;
    }

    if PARAMETERISED
        .iter()
        .any(|prefix| is_one_segment_under(path, prefix))
    {
        return true;
    }

    if PARAMETERISED_TAIL
        .iter()
        .any(|(prefix, tail)| is_one_segment_before(path, prefix, tail))
    {
        return true;
    }

    PARAMETERISED_PAIR
        .iter()
        .any(|prefix| is_two_segments_under(path, prefix))
}

/// Whether `path` is `prefix`, one non-empty segment, then `tail`.
fn is_one_segment_before(path: &str, prefix: &str, tail: &str) -> bool {
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(tail))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// Whether `path` is `prefix` followed by exactly two non-empty segments.
fn is_two_segments_under(path: &str, prefix: &str) -> bool {
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };

    match rest.split_once('/') {
        Some((first, second)) => !first.is_empty() && !second.is_empty() && !second.contains('/'),
        None => false,
    }
}

/// Whether `path` is `prefix` followed by exactly one non-empty segment.
fn is_one_segment_under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

#[cfg(test)]
mod tests {
    use actix_web::http::Method;
    use actix_web::http::header::CONTENT_TYPE;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;

    /// Every route this brief mounts, with the content type it must answer.
    ///
    /// The table is the contract: a route whose content type drifts breaks a
    /// client that compares the header with `===`, and nothing else in the test
    /// suite would notice.
    const ROUTES: &[(&str, &str, &str)] = &[
        ("GET", "/Marti/api/version", "text/plain"),
        ("GET", "/Marti/api/version/config", "application/json"),
        ("GET", "/Marti/api/version/info", "application/json"),
        ("GET", "/Marti/api/node/id", "text/plain"),
        ("GET", "/Marti/api/util/user/roles", "application/json"),
        ("GET", "/Marti/api/util/isAdmin", "application/json"),
        ("GET", "/Marti/api/home", "text/plain"),
        ("GET", "/Marti/api/video", "application/json"),
        ("GET", "/Marti/vcm", "application/xml"),
        ("GET", "/Marti/GetTime", "text/plain"),
        ("POST", "/Marti/ErrorLog", "text/plain"),
        ("GET", "/files/api/config", "application/json"),
    ];

    /// Routes that refuse, and the status each refusal carries.
    const REFUSALS: &[(&str, &str, u16)] = &[
        ("POST", "/Marti/api/video", 501),
        ("GET", "/Marti/api/video/abc", 404),
        ("GET", "/Marti/api/injectors/cot/uid", 401),
        ("GET", "/Marti/api/repeater/list", 401),
        ("GET", "/Marti/ExportMissionKML", 501),
        ("GET", "/Marti/KmlMasterSA", 501),
        ("GET", "/Marti/LatestKML", 501),
        ("GET", "/Marti/TracksKML", 501),
        ("GET", "/Marti/api/missions/Alpha/kml", 501),
        ("GET", "/Marti/vcu", 501),
        ("POST", "/Marti/sync/missioncreate", 501),
    ];

    #[actix_web::test]
    async fn every_route_answers_with_exactly_the_content_type_it_promised() {
        // node-tak compares this header with `===`; a `; charset=utf-8` suffix
        // sends it down a fallback branch that hands callers a raw string.
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for (method, uri, expected) in ROUTES {
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(method.parse().unwrap())
                    .uri(uri)
                    .to_request(),
            )
            .await;

            assert!(
                response.status().is_success(),
                "{method} {uri} answered {}",
                response.status(),
            );
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                expected,
                "{method} {uri}",
            );
        }
    }

    #[actix_web::test]
    async fn every_refusal_is_json_with_the_status_it_promised() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for (method, uri, status) in REFUSALS {
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(method.parse().unwrap())
                    .uri(uri)
                    .to_request(),
            )
            .await;

            assert_eq!(response.status().as_u16(), *status, "{method} {uri}");
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                "application/json",
                "{method} {uri}",
            );
        }
    }

    #[actix_web::test]
    async fn nothing_redirects_for_a_trailing_slash() {
        // A 301 to the canonical path is read by node-tak as a success carrying
        // an empty body, which fails somewhere else entirely.
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for (method, uri, _) in ROUTES {
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(method.parse().unwrap())
                    .uri(&format!("{uri}/"))
                    .to_request(),
            )
            .await;

            assert!(
                !response.status().is_redirection(),
                "{method} {uri}/ answered {}",
                response.status(),
            );
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                "application/json",
                "{method} {uri}/ must still refuse in our own shape",
            );
        }
    }

    #[actix_web::test]
    async fn a_method_a_route_does_not_serve_is_refused_rather_than_redirected() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for (_, uri, _) in ROUTES {
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(Method::PATCH)
                    .uri(uri)
                    .to_request(),
            )
            .await;

            assert!(
                !response.status().is_redirection(),
                "PATCH {uri} answered {}",
                response.status(),
            );
            assert!(
                matches!(response.status().as_u16(), 404 | 405),
                "PATCH {uri} answered {}",
                response.status(),
            );
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                "application/json",
                "PATCH {uri}",
            );
        }
    }

    #[actix_web::test]
    async fn an_unknown_marti_path_is_our_404_rather_than_the_single_page_shell() {
        // The application's catch-all serves the admin UI, which would answer a
        // missing Marti route with HTML and a 200.
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/nothing-here")
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), 404);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
        );
    }

    #[actix_web::test]
    async fn every_enveloped_route_carries_the_type_string_its_client_matches_on() {
        let server = TestServer::start().await;
        let (_, admin) = server.signed_in("grace", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let token = crate::testing::context::bearer(&admin);

        for (uri, expected, version) in [
            ("/Marti/api/version/config", "ServerConfig", "3"),
            ("/Marti/api/injectors/cot/uid", "UidCotTagInjector", "3"),
            ("/Marti/api/repeater/list", "Repeatable", "1.0.0"),
            ("/Marti/api/repeater/period", "Integer", "1.0.0"),
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(uri)
                    .insert_header(("authorization", token.clone()))
                    .to_request(),
            )
            .await;

            let body: serde_json::Value = test::read_body_json(response).await;

            assert_eq!(body["type"], expected, "{uri}");
            assert_eq!(body["version"], version, "{uri}");
            assert!(body["nodeId"].is_string(), "{uri}");
        }
    }

    #[actix_web::test]
    async fn the_method_table_covers_every_path_registered_under_more_than_one_verb() {
        // A path that serves two methods but is missing from the table answers
        // 404 where it means 405. Asserted directly so that adding a route
        // without its table entry fails here rather than confusing a client.
        for (_, path, _) in ROUTES {
            assert!(serves_path(path), "{path}");
        }

        for (_, path, _) in REFUSALS {
            assert!(serves_path(path), "{path}");
        }

        for path in [
            "/Marti/api/groups/all",
            "/Marti/api/groups/active",
            "/Marti/api/groups/Blue/IN",
            "/Marti/api/contacts/all",
            "/Marti/api/clientEndPoints",
            "/Marti/api/subscription/UID-A",
            "/Marti/api/subscriptions/all",
            "/Marti/api/cot",
            "/Marti/api/cot/sa",
            "/Marti/api/cot/matchUid",
            "/Marti/api/cot/xml/UID-A",
            "/Marti/api/cot/xml/UID-A/all",
            "/Marti/api/subscriptions/incognito/UID-A",
            "/Marti/api/subscriptions/delete/UID-A",
            "/Marti/api/subscriptions/UID-A/filter",
        ] {
            assert!(serves_path(path), "{path}");
        }

        for path in [
            "/Marti/api/nothing-here",
            "/Marti/api/videos",
            "/Marti/api/video/a/b",
            "/Marti/api/version/",
            "/Marti/api/missions/Alpha",
            "/Marti/api/missions//kml",
            "/Marti/api/groups/Blue/IN/extra",
            "/Marti/api/groups//IN",
            "/Marti/api/subscriptions//filter",
            "/Marti/api/subscriptions/UID-A/filters",
        ] {
            assert!(!serves_path(path), "{path}");
        }
    }
}
