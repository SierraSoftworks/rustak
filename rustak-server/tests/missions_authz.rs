//! Who may call what, over every mission route that is mounted.
//!
//! R-02 found three routes (C1, H4, H5) that had simply been written without a
//! permission check — `GET …/subscription?uid=` handed an **anonymous** caller a
//! `MISSION_OWNER` token, `DELETE …/subscription` let anybody unsubscribe
//! anybody, and the two log-entry-by-id routes read and deleted through a
//! password-protected mission. Each was a one-line omission in a file whose
//! every sibling was correct, so a test per finding would only have found the
//! three that had already been found.
//!
//! This suite therefore walks the **whole table** against four callers:
//!
//! | Caller | Expected |
//! |---|---|
//! | anonymous | `401`/`403` on everything but the deliberately public routes |
//! | a signed-in stranger | the same — an account is not a mission role |
//! | a read-only subscriber | reads succeed, writes are `403` |
//! | the owner | everything succeeds |
//!
//! A route added without a check fails here the day it is added. A route that is
//! *meant* to be reachable without a role is listed in [`PUBLIC`] with the reason,
//! so making one public is a deliberate edit to this file rather than an omission
//! somewhere else.
//!
//! # Why the fixture mission carries a password
//!
//! An ordinary mission hands its **default role** to anybody who asks
//! (`compat/missions.md` §5/§9), so an anonymous `GET …/subscriptions/roles`
//! against a public mission is a `200` by design and a matrix run against one
//! would assert nothing. A password-protected mission has no default role at
//! all, so "no role" really means none — which is the condition C1, H4 and H5
//! were all reachable under. The reader subscribes *before* the password is
//! set, so its role comes from its own subscription rather than the default.
//!
//! Run with `cargo test -p rustak-server --features testing --test missions_authz`.

#![cfg(feature = "testing")]

use actix_web::{App, http::Method, test};
use serde_json::Value;

use rustak_server::prelude::*;
use rustak_server::testing::TestServer;

/// The mission every case is run against.
const MISSION: &str = "Alpha";

/// The owner's device.
const OWNER_UID: &str = "ANDROID-owner";

/// The read-only subscriber's device.
const READER_UID: &str = "ANDROID-reader";

/// The password that takes the mission's default role away.
const PASSWORD: &str = "matrix-fixture";

/// One route, as the matrix walks it.
struct Route {
    method: Method,
    /// `{n}` is replaced with the mission name.
    path: &'static str,
    /// A body, for the routes that refuse an empty one before they check a role.
    body: Option<&'static str>,
    /// The lowest role that may call it.
    needs: Needs,
}

/// What a route asks of its caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Needs {
    /// `MISSION_READ` — a read-only subscriber is enough.
    Read,
    /// `MISSION_WRITE` or more — a read-only subscriber is refused.
    Write,
    /// Reachable with no role at all, for the reason in [`PUBLIC`].
    Public,
}

/// The routes that are deliberately reachable without a mission role.
///
/// Each is public because it is how a caller *acquires* a role, or because its
/// whole answer is "you have none".
const PUBLIC: &[(&str, &str)] = &[
    (
        "PUT /missions/{n}/subscription",
        "the password, invitation or token in the request is the credential (compat/missions.md §9)",
    ),
    (
        "GET /missions/{n}/token",
        "the password is the credential; a wrong one is the 403",
    ),
    (
        "GET /missions/{n}/role",
        "'what may I do here' has to be answerable by somebody who may do nothing",
    ),
    (
        "GET /missions/{n}",
        "API_VERSION >= 3 turns the refusal into a stripped 200 (compat/missions.md §5)",
    ),
];

/// Every mission route that hangs off one mission, with the role it needs.
///
/// Kept in the same order as `marti::missions::family` registers them, so that a
/// route added there and not here is visible in a diff.
fn routes() -> Vec<Route> {
    let read = |method: Method, path| Route {
        method,
        path,
        body: None,
        needs: Needs::Read,
    };
    let write = |method: Method, path| Route {
        method,
        path,
        body: None,
        needs: Needs::Write,
    };
    let write_body = |method: Method, path, body| Route {
        method,
        path,
        body: Some(body),
        needs: Needs::Write,
    };

    vec![
        read(Method::GET, "/missions/{n}/subscriptions/roles"),
        read(Method::GET, "/missions/{n}/subscriptions"),
        // C1: this used to mint and hand over a MISSION_OWNER token.
        read(Method::GET, "/missions/{n}/subscription?uid=ANDROID-reader"),
        // H4: this used to unsubscribe anybody from anything.
        read(
            Method::DELETE,
            "/missions/{n}/subscription?uid=ANDROID-nobody",
        ),
        Route {
            method: Method::POST,
            path: "/missions/{n}/subscription",
            body: Some("[]"),
            needs: Needs::Write,
        },
        write(
            Method::PUT,
            "/missions/{n}/role?clientUid=X&role=MISSION_SUBSCRIBER",
        ),
        write(Method::PUT, "/missions/{n}/password?password=secret"),
        write(Method::DELETE, "/missions/{n}/password"),
        write(Method::PUT, "/missions/{n}/expiration?expiration=-1"),
        write_body(
            Method::PUT,
            "/missions/{n}/contents",
            r#"{"uids":["UID-1"]}"#,
        ),
        write(Method::DELETE, "/missions/{n}/contents?uid=UID-1"),
        read(Method::GET, "/missions/{n}/changes"),
        read(Method::GET, "/missions/{n}/cot"),
        read(Method::GET, "/missions/{n}/children"),
        read(Method::GET, "/missions/{n}/parent"),
        write(Method::DELETE, "/missions/{n}/parent"),
        // `MISSION_READ`, not write: sending a mission to a contact creates an
        // invitation to a mission the caller may already read, which is what
        // `marti::missions::misc::send` has always asked for. R-02 did not
        // flag it; recorded here so that a change to it is a visible one.
        read(Method::POST, "/missions/{n}/send?contact=ANDROID-x"),
        read(Method::GET, "/missions/{n}/contacts"),
        read(Method::GET, "/missions/{n}/invitations"),
        write_body(Method::POST, "/missions/{n}/invite", "[]"),
        write(Method::PUT, "/missions/{n}/invite/clientUid/ANDROID-x"),
        write(Method::DELETE, "/missions/{n}/invite/clientUid/ANDROID-x"),
        read(Method::GET, "/missions/{n}/log"),
        write(
            Method::PUT,
            "/missions/{n}/layers/parent?uid=L1&parentUid=L2",
        ),
        write(Method::PUT, "/missions/{n}/layers/L1/name?name=renamed"),
        write(Method::PUT, "/missions/{n}/layers/L1/position?position=1"),
        read(Method::GET, "/missions/{n}/layers"),
        write(Method::PUT, "/missions/{n}/layers?name=L1&type=UID&uid=L1"),
        write(Method::DELETE, "/missions/{n}/layers?uid=L1"),
        write(Method::DELETE, "/missions/{n}/maplayers/ML1"),
        write_body(Method::POST, "/missions/{n}/maplayers", r#"{"name":"m"}"#),
        write(Method::DELETE, "/missions/{n}/externaldata/1"),
        write_body(
            Method::POST,
            "/missions/{n}/externaldata",
            r#"{"name":"e","tool":"t","uid":"E1","urlData":"u","urlView":"v"}"#,
        ),
        write(Method::DELETE, "/missions/{n}/feed/F1"),
        write_body(Method::POST, "/missions/{n}/feed", r#"{"feedUid":"F1"}"#),
        // The by-name-only family.
        write(Method::PUT, "/missions/{n}/contents/missionpackage"),
        write(Method::PUT, "/missions/{n}/uid/UID-1/keywords"),
        write(Method::DELETE, "/missions/{n}/uid/UID-1/keywords"),
        write(Method::PUT, "/missions/{n}/content/deadbeef/keywords"),
        write(Method::DELETE, "/missions/{n}/content/deadbeef/keywords"),
        write(Method::DELETE, "/missions/{n}/keywords/alpha"),
        write_body(Method::PUT, "/missions/{n}/keywords", r#"["alpha"]"#),
        write(Method::DELETE, "/missions/{n}/keywords"),
        read(Method::GET, "/missions/{n}/archive"),
        read(Method::PUT, "/missions/{n}/copy?name=Copied"),
        write(Method::PUT, "/missions/{n}/parent/Other"),
        // The mission itself.
        write(Method::DELETE, "/missions/{n}"),
        // The deliberately public four.
        Route {
            method: Method::PUT,
            path: "/missions/{n}/subscription?uid=ANDROID-new",
            body: None,
            needs: Needs::Public,
        },
        Route {
            method: Method::GET,
            path: "/missions/{n}/token?password=whatever",
            body: None,
            needs: Needs::Public,
        },
        Route {
            method: Method::GET,
            path: "/missions/{n}/role",
            body: None,
            needs: Needs::Public,
        },
        Route {
            method: Method::GET,
            path: "/missions/{n}",
            body: None,
            needs: Needs::Public,
        },
    ]
}

/// Whether a status is this suite's idea of "refused".
fn refused(status: u16) -> bool {
    status == 401 || status == 403
}

/// The full URI a route resolves to for this mission.
fn uri(route: &Route) -> String {
    format!("/Marti/api{}", route.path.replace("{n}", MISSION))
}

/// Issues one request, returning its status.
///
/// A macro rather than a function: `test::init_service` returns an opaque
/// `impl Service<actix_http::Request, …>` and naming that bound would mean a
/// dev-dependency on `actix-http` for one type.
macro_rules! status_of {
    ($app:expr, $route:expr, $authorization:expr) => {{
        let mut request = test::TestRequest::default()
            .method($route.method.clone())
            .uri(&uri($route));

        if let Some(value) = $authorization {
            request = request.insert_header(("authorization", format!("Bearer {value}")));
        }

        if let Some(body) = $route.body {
            request = request
                .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
                .set_payload(body);
        }

        test::call_service(&$app, request.to_request())
            .await
            .status()
            .as_u16()
    }};
}

/// Creates the mission, subscribes a read-only device, and hands back the two
/// session tokens plus the stranger's.
async fn fixtures(server: &TestServer) -> (String, String, String) {
    let (_, owner) = server.signed_in("grace", false).await;
    let (_, reader) = server.signed_in("mallory-reader", false).await;
    let (_, stranger) = server.signed_in("trent", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let created = test::call_service(
        &app,
        test::TestRequest::post()
            .uri(&format!(
                "/Marti/api/missions/{MISSION}?creatorUid={OWNER_UID}&defaultRole=MISSION_READONLY_SUBSCRIBER"
            ))
            .insert_header(("authorization", format!("Bearer {}", owner.token)))
            .to_request(),
    )
    .await;
    assert_eq!(created.status().as_u16(), 201);

    let subscribed = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!(
                "/Marti/api/missions/{MISSION}/subscription?uid={READER_UID}"
            ))
            .insert_header(("authorization", format!("Bearer {}", reader.token)))
            .to_request(),
    )
    .await;
    assert_eq!(
        subscribed.status().as_u16(),
        201,
        "the reader has to be subscribed for the read-only column to mean anything",
    );

    let protected = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!(
                "/Marti/api/missions/{MISSION}/password?password={PASSWORD}"
            ))
            .insert_header(("authorization", format!("Bearer {}", owner.token)))
            .to_request(),
    )
    .await;
    assert_eq!(
        protected.status().as_u16(),
        200,
        "a protected mission has no default role, which is what makes 'no role' mean none",
    );

    (owner.token, reader.token, stranger.token)
}

#[actix_web::test]
async fn an_anonymous_caller_is_refused_every_route_that_is_not_deliberately_public() {
    let server = TestServer::start().await;
    let (_, _, _) = fixtures(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for route in routes() {
        if route.needs == Needs::Public {
            continue;
        }

        let status = status_of!(app, &route, None::<&str>);

        assert!(
            refused(status),
            "{} {} answered {status} to an anonymous caller — every mission route \
             that is not in PUBLIC has to refuse one",
            route.method,
            uri(&route),
        );
    }

    assert!(
        !PUBLIC.is_empty(),
        "the public routes are documented rather than asserted uniformly: each \
         refuses on its own credential (a wrong password, a missing invitation) \
         rather than on a role, so they are covered by `missions_flow`",
    );
}

#[actix_web::test]
async fn a_signed_in_stranger_holds_no_role_on_somebody_elses_mission() {
    let server = TestServer::start().await;
    let (_, _, stranger) = fixtures(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let header = stranger;

    for route in routes() {
        if route.needs == Needs::Public {
            continue;
        }

        let status = status_of!(app, &route, Some(&header));

        assert!(
            refused(status),
            "{} {} answered {status} to an account with no subscription — an account \
             is not a mission role",
            route.method,
            uri(&route),
        );
    }
}

#[actix_web::test]
async fn a_read_only_subscriber_reads_and_cannot_write() {
    let server = TestServer::start().await;
    let (_, reader, _) = fixtures(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let header = reader;

    for route in routes() {
        if route.needs == Needs::Public {
            continue;
        }

        let status = status_of!(app, &route, Some(&header));

        match route.needs {
            Needs::Write => assert!(
                refused(status),
                "{} {} answered {status} to MISSION_READONLY_SUBSCRIBER",
                route.method,
                uri(&route),
            ),
            _ => assert!(
                !refused(status),
                "{} {} refused a read-only subscriber with {status}",
                route.method,
                uri(&route),
            ),
        }
    }
}

#[actix_web::test]
async fn the_owner_is_refused_nothing() {
    let server = TestServer::start().await;
    let (owner, _, _) = fixtures(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let header = owner;

    for route in routes() {
        // The delete would take the mission with it and every later case with
        // it; it is covered by `missions_flow`.
        if route.needs == Needs::Public
            || (route.method == Method::DELETE && route.path == "/missions/{n}")
        {
            continue;
        }

        let status = status_of!(app, &route, Some(&header));

        assert!(
            !refused(status),
            "{} {} refused the owner with {status}",
            route.method,
            uri(&route),
        );
    }
}

#[actix_web::test]
async fn reading_a_subscription_never_hands_over_a_token() {
    // R-02 C1. The endpoint used to mint a fresh SUBSCRIPTION token for whoever
    // asked; it now reports the stored row, and the token is minted by the
    // subscribe that proved something first.
    let server = TestServer::start().await;
    let (owner, _, _) = fixtures(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!(
                "/Marti/api/missions/{MISSION}/subscription?uid={READER_UID}"
            ))
            .insert_header(("authorization", format!("Bearer {owner}")))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    let body: Value = test::read_body_json(response).await;

    assert_eq!(body["data"]["clientUid"], READER_UID);
    assert!(
        body["data"]["token"].is_null(),
        "a read of somebody's subscription reports the role, never the key to it: {body}",
    );
}

#[actix_web::test]
async fn a_log_entry_cannot_be_read_or_deleted_by_a_stranger() {
    // R-02 H5: both used to answer from the id alone.
    let server = TestServer::start().await;
    let (owner, _, stranger) = fixtures(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let created = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/logs/entries")
            .insert_header(("authorization", format!("Bearer {owner}")))
            .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
            .set_payload(format!(
                r#"{{"content":"the log","creatorUid":"{OWNER_UID}","missionNames":["{MISSION}"]}}"#
            ))
            .to_request(),
    )
    .await;

    assert_eq!(created.status().as_u16(), 201);

    let body: Value = test::read_body_json(created).await;
    let id = body["data"]["id"].as_str().expect("the assigned id");

    for (method, who, expected) in [
        (Method::GET, None, true),
        (Method::DELETE, None, true),
        (Method::GET, Some(stranger.as_str()), true),
        (Method::DELETE, Some(stranger.as_str()), true),
        (Method::GET, Some(owner.as_str()), false),
    ] {
        let mut request = test::TestRequest::default()
            .method(method.clone())
            .uri(&format!("/Marti/api/missions/logs/entries/{id}"));

        if let Some(token) = who {
            request = request.insert_header(("authorization", format!("Bearer {token}")));
        }

        let status = test::call_service(&app, request.to_request())
            .await
            .status()
            .as_u16();

        assert_eq!(
            refused(status),
            expected,
            "{method} /missions/logs/entries/{{id}} answered {status} for {who:?}",
        );
    }
}

#[actix_web::test]
async fn a_token_for_a_deleted_mission_does_not_open_its_successor() {
    // R-01 (medium): the token check used to accept a match on the mission
    // **name or** the guid, so an `ACCESS` token minted for a mission that was
    // later deleted opened a brand new password-protected mission that reused
    // the name. A guid is never reused; a name is.
    let server = TestServer::start().await;
    let (_, owner) = server.signed_in("grace", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let bearer = format!("Bearer {}", owner.token);

    let created = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Recycled?creatorUid=ANDROID-owner")
            .insert_header(("authorization", bearer.clone()))
            .to_request(),
    )
    .await;
    assert_eq!(created.status().as_u16(), 201);

    let body: Value = test::read_body_json(created).await;
    let stale = body["data"][0]["token"]
        .as_str()
        .expect("the create token")
        .to_string();
    let first_guid = body["data"][0]["guid"].as_str().unwrap().to_string();

    let deleted = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri("/Marti/api/missions/Recycled")
            .insert_header(("authorization", bearer.clone()))
            .to_request(),
    )
    .await;
    assert_eq!(deleted.status().as_u16(), 200);

    // What `jobs::mission_expiry::purge_tombstones` does once the tombstone is
    // past its horizon: the row goes, and the name is free for a new mission.
    server
        .context
        .db()
        .write(|tx| {
            tx.execute(
                "DELETE FROM missions WHERE deleted_at IS NOT NULL",
                rusqlite::params![],
            )
        })
        .await
        .expect("purge the tombstone");

    let recreated = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Recycled?creatorUid=ANDROID-other&password=locked")
            .insert_header(("authorization", bearer))
            .to_request(),
    )
    .await;
    assert_eq!(recreated.status().as_u16(), 201, "the name is free again");

    let body: Value = test::read_body_json(recreated).await;
    assert_ne!(
        body["data"][0]["guid"].as_str().unwrap(),
        first_guid,
        "a recreated mission is a different mission",
    );

    // The stale token names the *old* guid, so it opens nothing.
    let replayed = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Recycled/subscriptions/roles")
            .insert_header(("MissionAuthorization", format!("Bearer {stale}")))
            .to_request(),
    )
    .await;

    assert!(
        refused(replayed.status().as_u16()),
        "a token minted for a deleted mission answered {} against its successor",
        replayed.status().as_u16(),
    );
}
