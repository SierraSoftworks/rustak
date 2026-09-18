//! The Marti wire contract, asserted against the application the listener
//! serves.
//!
//! The unit tests inside `src/marti/**` each check one module. This suite
//! checks the thing they add up to: the real `App` from
//! `web::server::services`, with its middleware, its catch-all and its
//! extractor error handlers in place — because every failure this file exists
//! to catch is a *wiring* failure, and a route tested in isolation cannot see
//! any of them.
//!
//! # What breaks if these fail
//!
//! * **Content type.** node-tak, the client library CloudTAK is written
//!   against, decides whether a response is JSON with `header === 'application/json'`.
//!   A `; charset=utf-8` suffix — which actix appends on several paths that
//!   look like the ones we use — sends it down a fallback branch that hands the
//!   caller a raw string. Most of its call sites index straight into the result
//!   and throw a `TypeError` somewhere else entirely.
//! * **A redirect.** Every status below 400 is success to node-tak, which then
//!   parses the redirect's empty body as the payload. A trailing slash must
//!   therefore be a `404`, never a `301` to the path that would have worked.
//! * **An envelope `type` string.** ATAK matches these as literals and rejects
//!   a whole response over an unexpected one.
//! * **`/files/api/config`.** CloudTAK's setup wizard cannot save a server
//!   connection until it answers with an integer `uploadSizeLimit`.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use actix_web::http::Method;
use actix_web::http::header::CONTENT_TYPE;
use actix_web::{App, test};
use rustak_server::testing::TestServer;

/// Every route this milestone mounts, with the content type it must answer.
///
/// The table is the contract. A route added without a line here is a route no
/// client's parser has been checked against.
const CONTRACT: &[(&str, &str, u16, &str)] = &[
    ("GET", "/Marti/api/version", 200, "text/plain"),
    ("GET", "/Marti/api/version/config", 200, "application/json"),
    ("GET", "/Marti/api/version/info", 200, "application/json"),
    ("GET", "/Marti/api/node/id", 200, "text/plain"),
    ("GET", "/Marti/api/util/user/roles", 200, "application/json"),
    ("GET", "/Marti/api/util/isAdmin", 200, "application/json"),
    ("GET", "/Marti/api/home", 200, "text/plain"),
    ("GET", "/Marti/api/video", 200, "application/json"),
    ("POST", "/Marti/api/video", 501, "application/json"),
    ("GET", "/Marti/api/video/abc", 404, "application/json"),
    ("PUT", "/Marti/api/video/abc", 501, "application/json"),
    ("DELETE", "/Marti/api/video/abc", 501, "application/json"),
    (
        "GET",
        "/Marti/api/injectors/cot/uid",
        401,
        "application/json",
    ),
    ("GET", "/Marti/api/repeater/list", 401, "application/json"),
    ("GET", "/Marti/api/repeater/period", 401, "application/json"),
    (
        "GET",
        "/Marti/api/missions/Alpha/kml",
        501,
        "application/json",
    ),
    // Channels, contacts and subscriptions: every one of them needs a
    // credential, so an unauthenticated probe asserts the refusal shape rather
    // than the payload. The payloads are asserted in `tests/marti_channels.rs`
    // against a real listener with real clients on it.
    ("GET", "/Marti/api/groups/all", 401, "application/json"),
    ("PUT", "/Marti/api/groups/active", 401, "application/json"),
    (
        "PUT",
        "/Marti/api/groups/activebits",
        401,
        "application/json",
    ),
    (
        "GET",
        "/Marti/api/groups/groupCacheEnabled",
        200,
        "application/json",
    ),
    ("GET", "/Marti/api/groups/user", 401, "application/json"),
    ("GET", "/Marti/api/groups/Blue/IN", 401, "application/json"),
    ("GET", "/Marti/api/users/all", 401, "application/json"),
    ("GET", "/Marti/api/contacts/all", 401, "application/json"),
    ("GET", "/Marti/api/clientEndPoints", 401, "application/json"),
    (
        "GET",
        "/Marti/api/subscriptions/all",
        401,
        "application/json",
    ),
    (
        "GET",
        "/Marti/api/subscription/UID-A",
        401,
        "application/json",
    ),
    (
        "POST",
        "/Marti/api/subscriptions/incognito/UID-A",
        401,
        "application/json",
    ),
    (
        "DELETE",
        "/Marti/api/subscriptions/delete/UID-A",
        401,
        "application/json",
    ),
    (
        "PUT",
        "/Marti/api/subscriptions/UID-A/filter",
        401,
        "application/json",
    ),
    ("GET", "/Marti/GetTime", 200, "text/plain"),
    ("POST", "/Marti/ErrorLog", 200, "text/plain"),
    ("GET", "/Marti/vcm", 200, "application/xml"),
    ("GET", "/Marti/vcu", 501, "application/json"),
    ("POST", "/Marti/vcs", 501, "application/json"),
    ("GET", "/Marti/ExportMissionKML", 501, "application/json"),
    ("GET", "/Marti/KmlMasterSA", 501, "application/json"),
    ("GET", "/Marti/LatestKML", 501, "application/json"),
    ("GET", "/Marti/TracksKML", 501, "application/json"),
    ("POST", "/Marti/sync/missioncreate", 501, "application/json"),
    ("GET", "/files/api/config", 200, "application/json"),
];

/// Builds the application the public listener serves.
macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// One request, as `(status, content type, body)`.
///
/// A macro rather than a function because `test::init_service` returns an
/// opaque `Service` over `actix_http::Request`, and naming that bound would
/// mean depending on a crate this one reaches only through `actix-web`.
macro_rules! call {
    ($app:expr, $method:expr, $uri:expr) => {{
        let request = test::TestRequest::default()
            .method(Method::from_bytes($method.as_bytes()).expect("a real method"))
            .uri($uri)
            .to_request();

        let response = test::call_service(&$app, request).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map_or_else(String::new, |value| {
                value.to_str().unwrap_or_default().to_string()
            });
        let body = String::from_utf8(test::read_body(response).await.to_vec()).expect("utf-8");

        (status, content_type, body)
    }};
}

#[actix_web::test]
async fn every_route_answers_with_the_status_and_content_type_it_promised() {
    // The single most fragile detail in the whole surface: node-tak compares
    // the content type with `===`, so a charset parameter is correct HTTP and a
    // broken client.
    let server = TestServer::start().await;
    let app = app!(server);

    for (method, uri, status, content_type) in CONTRACT {
        let (got_status, got_type, _) = call!(app, method, uri);

        assert_eq!(got_status, *status, "{method} {uri}");
        assert_eq!(got_type, *content_type, "{method} {uri}");
        assert!(
            !got_type.contains("charset"),
            "{method} {uri} answered {got_type}, which node-tak will not parse",
        );
    }
}

#[actix_web::test]
async fn nothing_in_the_marti_surface_ever_redirects() {
    // The four ways a server usually redirects — a trailing slash, a doubled
    // slash, a dot segment, and a method it does not serve — against every
    // route. node-tak reads any of them as a success carrying no body.
    let server = TestServer::start().await;
    let app = app!(server);

    for (method, uri, _, _) in CONTRACT {
        for probe in [format!("{uri}/"), format!("{uri}//"), format!("{uri}/.")] {
            let (status, _, _) = call!(app, method, &probe);

            assert!(
                !(300..400).contains(&status),
                "{method} {probe} answered {status}",
            );
        }

        for other in ["PATCH", "TRACE"] {
            let (status, content_type, _) = call!(app, other, uri);

            assert!(
                !(300..400).contains(&status),
                "{other} {uri} answered {status}",
            );
            assert_eq!(
                content_type, "application/json",
                "{other} {uri} must refuse in our own shape",
            );
        }
    }
}

#[actix_web::test]
async fn an_unmatched_marti_path_is_our_json_rather_than_the_admin_ui() {
    // The application's catch-all serves the single-page shell, so a Marti
    // route that was never mounted would otherwise be HTML and a `200` — which
    // a TAK client would try to parse as a mission list.
    let server = TestServer::start().await;
    let app = app!(server);

    for uri in [
        "/Marti/api/nothing-here",
        "/Marti/nothing-here",
        "/Marti/api/version/nothing-here",
        "/files/api/nothing-here",
    ] {
        let (status, content_type, body) = call!(app, "GET", uri);

        assert_eq!(status, 404, "{uri}");
        assert_eq!(content_type, "application/json", "{uri}");

        let parsed: serde_json::Value = serde_json::from_str(&body).expect(uri);
        assert_eq!(parsed["status"], "NOT_FOUND", "{uri}");
        assert_eq!(parsed["code"], 1, "{uri}");
    }
}

#[actix_web::test]
async fn every_refusal_carries_all_three_fields_of_the_error_shape() {
    // A client reading `body.message` must find a string rather than
    // `undefined`, even where the server had nothing to say.
    let server = TestServer::start().await;
    let app = app!(server);

    for (method, uri, status, _) in CONTRACT.iter().filter(|(_, _, status, _)| *status >= 400) {
        let (_, _, body) = call!(app, method, uri);
        let parsed: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|_| panic!("{method} {uri}: {body}"));

        assert!(parsed["status"].is_string(), "{method} {uri}");
        assert!(parsed["code"].is_u64(), "{method} {uri}");
        assert!(parsed["message"].is_string(), "{method} {uri}");
        assert_eq!(
            parsed["status"],
            match status {
                401 => "UNAUTHORIZED",
                404 => "NOT_FOUND",
                501 => "NOT_IMPLEMENTED",
                other => panic!("{method} {uri} answered an unexpected {other}"),
            },
            "{method} {uri}",
        );
    }
}

#[actix_web::test]
async fn the_server_config_is_what_ataks_version_parser_reads() {
    // ATAK parses the *envelope's* `version` as an integer to decide whether
    // the server is new enough for `tool` on a sync search, string-matches
    // `type`, and reads `data.version` as this server's own version. Swapping
    // the two silently disables a feature.
    let server = TestServer::start().await;
    let app = app!(server);

    let request = test::TestRequest::get()
        .uri("/Marti/api/version/config")
        .insert_header(("host", "tak.example.com:8443"))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, request).await;

    assert_eq!(body["type"], "ServerConfig");
    assert_eq!(
        body["version"].as_str().and_then(|v| v.parse::<i32>().ok()),
        Some(3),
        "{body}",
    );
    assert!(body["data"]["version"].is_string(), "{body}");
    assert_eq!(body["data"]["api"], "3");
    assert_eq!(body["data"]["hostname"], "tak.example.com");
    assert!(body["nodeId"].is_string(), "{body}");
    assert!(body.get("data").is_some(), "{body}");
}

#[actix_web::test]
async fn the_version_probe_is_the_product_string_atak_matches_on() {
    let server = TestServer::start().await;
    let app = app!(server);

    let (status, content_type, body) = call!(app, "GET", "/Marti/api/version");

    assert_eq!(status, 200);
    assert_eq!(content_type, "text/plain");
    assert!(body.starts_with("TAK Server "), "{body}");
    assert!(body.contains("rustak-"), "{body}");
    assert!(!body.ends_with('\n'), "{body:?}");
}

#[actix_web::test]
async fn the_cloudtak_setup_gate_answers_before_anything_is_configured() {
    // `PATCH /api/server`, CloudTAK's own setup call, validates a newly entered
    // server by calling exactly this and checking `uploadSizeLimit !==
    // undefined`. Until it answers, the wizard cannot save a connection at all.
    let server = TestServer::start().await;
    let app = app!(server);

    let (status, content_type, body) = call!(app, "GET", "/files/api/config");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(status, 200);
    assert_eq!(content_type, "application/json");
    assert!(parsed["uploadSizeLimit"].is_u64(), "{body}");
    assert!(
        !body.contains("\"type\""),
        "this one is not enveloped: {body}",
    );
}

#[actix_web::test]
async fn the_enveloped_routes_carry_the_type_strings_their_clients_match_on() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    for (uri, kind, version) in [
        ("/Marti/api/version/config", "ServerConfig", "3"),
        ("/Marti/api/injectors/cot/uid", "UidCotTagInjector", "3"),
        ("/Marti/api/repeater/list", "Repeatable", "1.0.0"),
        ("/Marti/api/repeater/period", "Integer", "1.0.0"),
        (
            "/Marti/api/repeater/remove/abc",
            "java.lang.Boolean",
            "1.0.0",
        ),
    ] {
        let request = test::TestRequest::get()
            .uri(uri)
            .insert_header(("authorization", token.clone()))
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, request).await;

        assert_eq!(body["type"], kind, "{uri}");
        assert_eq!(body["version"], version, "{uri}");
        assert!(body["nodeId"].is_string(), "{uri}");
    }
}

#[actix_web::test]
async fn the_unenveloped_routes_are_left_unenveloped() {
    // Three endpoints deliberately break the envelope convention, and a client
    // that expects a bare value would choke on one. Do not "fix" them.
    let server = TestServer::start().await;
    let app = app!(server);

    let (_, _, roles) = call!(app, "GET", "/Marti/api/util/user/roles");
    assert!(roles.starts_with('['), "{roles}");

    let (_, _, is_admin) = call!(app, "GET", "/Marti/api/util/isAdmin");
    assert_eq!(is_admin, "false");

    let (_, _, video) = call!(app, "GET", "/Marti/api/video");
    assert_eq!(video, r#"{"videoConnections":[]}"#);

    for body in [&roles, &is_admin, &video] {
        assert!(!body.contains("\"nodeId\""), "{body}");
    }
}

#[actix_web::test]
async fn roles_report_read_only_from_channel_membership_rather_than_a_flag() {
    let server = TestServer::start().await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let anonymous: Vec<String> =
        serde_json::from_str(&call!(app, "GET", "/Marti/api/util/user/roles").2).unwrap();

    assert_eq!(
        anonymous,
        ["ROLE_ANONYMOUS", "ROLE_WEBTAK", "ROLE_READONLY"]
    );

    for (session, expected) in [
        (&ordinary, vec!["ROLE_ANONYMOUS", "ROLE_WEBTAK"]),
        (&admin, vec!["ROLE_ANONYMOUS", "ROLE_ADMIN", "ROLE_WEBTAK"]),
    ] {
        let request = test::TestRequest::get()
            .uri("/Marti/api/util/user/roles")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request();
        let roles: Vec<String> = test::call_and_read_body_json(&app, request).await;

        assert_eq!(roles, expected);
    }
}

#[actix_web::test]
async fn the_api_version_header_reaches_the_handlers_in_either_spelling() {
    // Absent means 2, which is what a client that has never heard of the header
    // gets; junk means 2 as well, rather than a 400 on every call it makes.
    let server = TestServer::start().await;
    let app = app!(server);

    for value in ["3", "2", "three", ""] {
        let request = test::TestRequest::get()
            .uri("/Marti/api/util/isAdmin")
            .insert_header(("api_version", value))
            .to_request();
        let response = test::call_service(&app, request).await;

        assert_eq!(response.status(), 200, "API_VERSION: {value:?}");
    }
}

#[actix_web::test]
async fn a_preflight_is_refused_until_an_operator_asks_for_cross_origin_access() {
    // ATAK and CloudTAK's server are not browsers and never send an `Origin`,
    // so the default leaves every Marti response unreadable to a page the
    // operator's users happen to be on.
    let server = TestServer::start().await;
    let app = app!(server);

    let (status, content_type, _) = call!(app, "OPTIONS", "/Marti/api/version");

    assert_eq!(status, 405);
    assert_eq!(content_type, "application/json");

    let permissive = TestServer::start_with(|config| {
        config.marti.allow_all_origins = true;
    })
    .await;
    let app = app!(permissive);

    let request = test::TestRequest::default()
        .method(Method::OPTIONS)
        .uri("/Marti/api/version")
        .to_request();
    let response = test::call_service(&app, request).await;

    assert_eq!(response.status(), 204);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "*",
    );
}

#[actix_web::test]
async fn a_crash_report_is_accepted_whether_or_not_it_is_kept() {
    // A client that cannot file one retries, so the answer is `200` either way;
    // what the setting decides is whether the body is stored.
    for store in [true, false] {
        let server = TestServer::start_with(move |config| {
            config.marti.store_error_logs = store;
        })
        .await;
        let app = app!(server);

        let request = test::TestRequest::post()
            .uri("/Marti/ErrorLog?clientUid=ANDROID-1")
            .set_payload("java.lang.IllegalStateException")
            .to_request();
        let response = test::call_service(&app, request).await;

        assert_eq!(response.status(), 200, "store_error_logs = {store}");
    }
}

#[actix_web::test]
async fn the_admin_api_is_untouched_by_the_marti_scope() {
    // Both are mounted on the same listener; a prefix collision would show up
    // as the admin API answering in the Marti error shape, or the other way
    // round.
    let server = TestServer::start().await;
    let app = app!(server);

    let (status, content_type, _) = call!(app, "GET", "/api/v1/health");
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/json");

    let (status, _, _) = call!(app, "GET", "/api/v1/me");
    assert_eq!(
        status, 401,
        "the admin gate still refuses without a session"
    );

    let (status, _, _) = call!(app, "GET", "/robots.txt");
    assert_eq!(status, 200);
}
