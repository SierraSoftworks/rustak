//! Data Sync end to end, against the real application.
//!
//! The unit tests under `src/missions/**` check each rule in isolation; this
//! suite checks the thing they add up to — the routes as they are mounted, with
//! the middleware, the extractors and the default service in place — and holds
//! every payload against the JSON-schema oracle in `tests/fixtures/`, which is
//! transcribed from the TypeBox types CloudTAK's client library validates
//! against.
//!
//! # What breaks if these fail
//!
//! * **`201` with a `token` on create.** CloudTAK persists `data[0].token` and
//!   `data[0].guid` straight off the create response and has no other way to
//!   manage the Data Sync afterwards. A create that answered `200`, or one with
//!   no token, leaves it holding a mission it cannot edit.
//! * **The two `type` strings.** The singular subscription route answers the
//!   fully-qualified Java class name and the plural one answers the bare
//!   literal. A client that switches on `type` rejects the other.
//! * **`410` after a delete.** A device still holding a mission token has to be
//!   told to forget the mission rather than to try another spelling.
//! * **Always-present arrays.** `uids`, `contents`, `externalData`, `feeds` and
//!   `mapLayers` are non-optional in CloudTAK's schema; a mission that omitted
//!   one because it was empty fails validation there rather than here.
//!
//! Run with `cargo test -p rustak-server --features testing --test missions_flow`.

#![cfg(feature = "testing")]

mod schema;

use actix_web::http::header::CONTENT_TYPE;
use actix_web::web::Bytes;
use actix_web::{App, test};
use serde_json::Value;

use rustak_server::testing::TestServer;

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// The device the owner creates and subscribes from.
const OWNER_UID: &str = "ANDROID-owner";

/// The envelope's `data`, with the content type asserted.
macro_rules! envelope {
    ($app:expr, $request:expr) => {{
        let response = test::call_service(&$app, $request).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body: Value = test::read_body_json(response).await;

        (status, content_type, body)
    }};
}

/// Signs in and creates a mission, returning the app, the session and the
/// created payload.
async fn created(server: &TestServer, name: &str) -> (String, Value) {
    let (_, session) = server.signed_in("grace", false).await;
    let app = app!(server);

    let (status, content_type, body) = envelope!(
        app,
        test::TestRequest::post()
            .uri(&format!(
                "/Marti/api/missions/{name}?creatorUid={OWNER_UID}&description=the%20golden%20mission"
            ))
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request()
    );

    assert_eq!(status, 201, "a create answers 201: {body}");
    assert_eq!(content_type, "application/json");

    (session.token, body)
}

#[actix_web::test]
async fn a_create_answers_201_with_a_token_and_matches_the_schema() {
    let server = TestServer::start().await;
    let (_, body) = created(&server, "Alpha%20Team").await;

    assert_eq!(body["type"], "Mission");

    let mission = &body["data"][0];

    schema::validate("mission", mission);

    assert_eq!(mission["name"], "Alpha Team");
    assert_eq!(mission["description"], "the golden mission");
    assert!(
        mission["token"].is_string(),
        "CloudTAK reads the token off the create response",
    );
    assert_eq!(mission["ownerRole"]["type"], "MISSION_OWNER");
    assert_eq!(
        mission["ownerRole"]["permissions"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(mission["defaultRole"]["type"], "MISSION_SUBSCRIBER");
    assert_eq!(mission["expiration"], -1);
    assert_eq!(mission["passwordProtected"], false);
    assert_eq!(mission["inviteOnly"], false);
    assert_eq!(mission["groups"], serde_json::json!(["__ANON__"]));

    for always in ["uids", "contents", "externalData", "feeds", "mapLayers"] {
        assert!(
            mission[always].is_array(),
            "{always} is non-optional in CloudTAK's schema",
        );
    }
}

#[actix_web::test]
async fn the_created_mission_matches_its_golden() {
    let server = TestServer::start().await;
    let (_, body) = created(&server, "Alpha%20Team").await;

    let golden: Value =
        serde_json::from_str(include_str!("fixtures/mission.golden.json")).expect("the golden");

    assert_eq!(schema::normalise(&body["data"][0]), golden);
}

#[actix_web::test]
async fn a_second_create_of_the_same_name_is_an_update_with_no_token() {
    let server = TestServer::start().await;
    let (token, _) = created(&server, "Alpha%20Team").await;
    let app = app!(server);

    let (status, _, body) = envelope!(
        app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Alpha%20Team?description=changed")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 200, "an update answers 200: {body}");
    assert!(
        body["data"][0]["token"].is_null(),
        "an update carries no token, which is how CloudTAK tells the two apart",
    );
    assert_eq!(body["data"][0]["description"], "changed");
}

#[actix_web::test]
async fn a_mission_is_reachable_by_guid_and_by_name() {
    let server = TestServer::start().await;
    let (token, created) = created(&server, "Alpha%20Team").await;
    let guid = created["data"][0]["guid"].as_str().unwrap().to_string();
    let app = app!(server);

    for uri in [
        format!("/Marti/api/missions/guid/{guid}"),
        "/Marti/api/missions/Alpha%20Team".to_string(),
    ] {
        let (status, _, body) = envelope!(
            app,
            test::TestRequest::get()
                .uri(&uri)
                .insert_header(("authorization", format!("Bearer {token}")))
                .to_request()
        );

        assert_eq!(status, 200, "{uri} answered {status}: {body}");
        assert_eq!(body["data"][0]["name"], "Alpha Team");
        assert!(body["data"][0]["token"].is_null(), "only a create has one");
    }
}

#[actix_web::test]
async fn a_uuid_shaped_name_is_refused() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("grace", false).await;
    let app = app!(server);

    let (status, content_type, body) = envelope!(
        app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/7e57d004-2b97-0e7a-b45f-5387367791cd")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request()
    );

    assert_eq!(status, 400, "{body}");
    assert_eq!(content_type, "application/json");
    assert_eq!(body["code"], 5);
}

#[actix_web::test]
async fn subscribing_answers_201_with_a_token_and_the_fully_qualified_type() {
    let server = TestServer::start().await;
    let (token, _) = created(&server, "Alpha%20Team").await;
    let app = app!(server);

    let (status, _, body) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/subscription?uid=ANDROID-2")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 201, "{body}");
    assert_eq!(body["type"], "com.bbn.marti.sync.model.MissionSubscription");
    schema::validate("mission_subscription", &body["data"]);
    assert!(body["data"]["token"].is_string());
    assert_eq!(body["data"]["clientUid"], "ANDROID-2");

    let (status, _, plural) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/subscriptions")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 200);
    assert_eq!(
        plural["type"], "MissionSubscription",
        "the plural route uses the bare literal, not the FQCN",
    );
    assert!(
        plural["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|uid| uid == "ANDROID-2"),
        "{plural}",
    );

    let (_, _, roles) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/subscriptions/roles")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    for subscription in roles["data"].as_array().unwrap() {
        schema::validate("mission_subscription", subscription);
        assert!(
            subscription["token"].is_null(),
            "the role listing never carries tokens: {subscription}",
        );
    }

    let (status, _, _) = envelope!(
        app,
        test::TestRequest::delete()
            .uri("/Marti/api/missions/Alpha%20Team/subscription?uid=ANDROID-2")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 200);
}

#[actix_web::test]
async fn a_subscription_token_alone_grants_the_subscriptions_role() {
    let server = TestServer::start().await;
    let (_, created) = created(&server, "Alpha%20Team").await;
    let mission_token = created["data"][0]["token"].as_str().unwrap().to_string();
    let app = app!(server);

    // No identity at all — only the mission token, in the header CloudTAK uses.
    let (status, _, body) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/role")
            .insert_header(("MissionAuthorization", format!("Bearer {mission_token}")))
            .to_request()
    );

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["type"], "MISSION_OWNER");
}

#[actix_web::test]
async fn attaching_and_detaching_a_file_shows_up_in_the_changes() {
    let server = TestServer::start().await;
    let (token, _) = created(&server, "Alpha%20Team").await;
    let app = app!(server);

    let upload = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=notes.txt&creatorUid=ANDROID-owner")
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"mission attachment"))
            .to_request(),
    )
    .await;

    assert_eq!(upload.status().as_u16(), 200);

    let metadata: Value = test::read_body_json(upload).await;
    let hash = metadata["Hash"].as_str().unwrap().to_string();

    let (status, _, attached) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/contents?creatorUid=ANDROID-owner")
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header((CONTENT_TYPE, "application/json"))
            .set_payload(format!(r#"{{"hashes":["{hash}"]}}"#))
            .to_request()
    );

    assert_eq!(status, 200, "{attached}");
    assert_eq!(attached["data"][0]["contents"].as_array().unwrap().len(), 1);
    schema::validate("resource", &attached["data"][0]["contents"][0]["data"]);

    let (_, _, changes) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/changes")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(changes["type"], "MissionChange");

    for change in changes["data"].as_array().unwrap() {
        schema::validate("mission_change", change);
    }

    assert!(
        changes["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["type"] == "ADD_CONTENT"),
        "{changes}",
    );

    let (status, _, _) = envelope!(
        app,
        test::TestRequest::delete()
            .uri(&format!(
                "/Marti/api/missions/Alpha%20Team/contents?hash={hash}"
            ))
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 200);

    // Squashed: the add is gone because the file is gone, and the remove is
    // kept because it is.
    let (_, _, squashed) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/changes?squashed=true")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );
    let kinds: Vec<&str> = squashed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["type"].as_str().unwrap())
        .collect();

    assert!(kinds.contains(&"REMOVE_CONTENT"), "{squashed}");
    assert!(!kinds.contains(&"ADD_CONTENT"), "{squashed}");

    // Full history keeps both.
    let (_, _, full) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/changes?squashed=false")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );
    let kinds: Vec<&str> = full["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["type"].as_str().unwrap())
        .collect();

    assert!(kinds.contains(&"ADD_CONTENT"), "{full}");
    assert!(kinds.contains(&"REMOVE_CONTENT"), "{full}");
    assert!(kinds.contains(&"CREATE_MISSION"), "{full}");
}

#[actix_web::test]
async fn a_password_protected_mission_hands_out_an_access_token() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("grace", false).await;
    let app = app!(server);

    let (status, _, body) = envelope!(
        app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Locked?password=hunter2&creatorUid=ANDROID-owner")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request()
    );

    assert_eq!(status, 201, "{body}");
    assert_eq!(body["data"][0]["passwordProtected"], true);

    let (status, _, refused) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Locked/token?password=wrong")
            .to_request()
    );

    assert_eq!(status, 403, "{refused}");

    let (status, _, minted) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Locked/token?password=hunter2")
            .to_request()
    );

    assert_eq!(status, 201, "a minted token answers 201: {minted}");
    assert_eq!(minted["type"], "java.lang.String");
    assert!(minted["data"].as_str().unwrap().contains('.'), "{minted}");

    // The access token grants the default role, and nothing else does on a
    // password-protected mission.
    let (status, _, role) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Locked/role")
            .insert_header((
                "MissionAuthorization",
                format!("Bearer {}", minted["data"].as_str().unwrap()),
            ))
            .to_request()
    );

    assert_eq!(status, 200);
    assert_eq!(role["data"]["type"], "MISSION_SUBSCRIBER", "{role}");
}

#[actix_web::test]
async fn deleting_by_guid_makes_every_later_read_a_410() {
    let server = TestServer::start().await;
    let (token, created) = created(&server, "Alpha%20Team").await;
    let guid = created["data"][0]["guid"].as_str().unwrap().to_string();
    let app = app!(server);

    let (status, _, body) = envelope!(
        app,
        test::TestRequest::delete()
            .uri(&format!("/Marti/api/missions?guid={guid}"))
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"][0]["name"], "Alpha Team");

    for uri in [
        format!("/Marti/api/missions/guid/{guid}"),
        "/Marti/api/missions/Alpha%20Team".to_string(),
    ] {
        let (status, content_type, gone) = envelope!(
            app,
            test::TestRequest::get()
                .uri(&uri)
                .insert_header(("authorization", format!("Bearer {token}")))
                .to_request()
        );

        assert_eq!(status, 410, "{uri} answered {status}: {gone}");
        assert_eq!(content_type, "application/json");
        assert_eq!(gone["code"], 8);
    }

    let (_, _, listing) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert!(
        listing["data"].as_array().unwrap().is_empty(),
        "a deleted mission is gone from the listing: {listing}",
    );
}

#[actix_web::test]
async fn deleting_without_a_guid_carries_taks_own_wording() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("grace", false).await;
    let app = app!(server);

    let (status, _, body) = envelope!(
        app,
        test::TestRequest::delete()
            .uri("/Marti/api/missions?guid=not-a-uuid")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request()
    );

    assert_eq!(status, 400);
    assert_eq!(
        body["message"], "Invalid Request: Invalid mission guid in request",
        "CloudTAK matches on this string",
    );
}

#[actix_web::test]
async fn an_empty_mission_still_answers_a_cot_document() {
    let server = TestServer::start().await;
    let (token, _) = created(&server, "Alpha%20Team").await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/cot")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/xml",
    );

    let body = test::read_body(response).await;
    let text = String::from_utf8_lossy(&body);

    assert!(text.starts_with("<?xml version='1.0'"), "{text}");
    assert!(text.ends_with("</events>"), "{text}");
}

#[actix_web::test]
async fn keywords_are_replaced_and_removed_one_at_a_time() {
    let server = TestServer::start().await;
    let (token, _) = created(&server, "Alpha%20Team").await;
    let app = app!(server);

    let (status, _, set) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/keywords")
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header((CONTENT_TYPE, "application/json"))
            .set_payload(r#"["alpha"," bravo ","ALPHA"]"#)
            .to_request()
    );

    assert_eq!(status, 200, "{set}");
    assert_eq!(
        set["data"][0]["keywords"],
        serde_json::json!(["alpha", "bravo"]),
        "keywords are trimmed and deduplicated",
    );

    let (status, _, removed) = envelope!(
        app,
        test::TestRequest::delete()
            .uri("/Marti/api/missions/Alpha%20Team/keywords/alpha")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request()
    );

    assert_eq!(status, 200, "{removed}");
    assert_eq!(removed["data"][0]["keywords"], serde_json::json!(["bravo"]));
}

#[actix_web::test]
async fn the_rest_of_the_family_is_wired_and_extracts_its_parameters() {
    // Every route below takes a path parameter or two, and an extractor that
    // does not match its route is a `500` at request time rather than a
    // compile error — which is exactly the failure this walk exists to catch.
    let server = TestServer::start().await;
    let (token, created) = created(&server, "Alpha%20Team").await;
    let guid = created["data"][0]["guid"].as_str().unwrap().to_string();
    let app = app!(server);

    let bearer = format!("Bearer {token}");

    // The paged listing and the count, which default their two flags the other
    // way round from the unpaged listing.
    for uri in [
        "/Marti/api/pagedmissions?pagesize=5",
        "/Marti/api/missioncount",
    ] {
        let (status, content_type, body) = envelope!(
            app,
            test::TestRequest::get()
                .uri(uri)
                .insert_header(("authorization", bearer.clone()))
                .to_request()
        );

        assert_eq!(status, 200, "{uri} answered {status}: {body}");
        assert_eq!(content_type, "application/json");
    }

    // A copy, and then the parent/child tree between the two.
    let (status, _, copied) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/copy?copyName=Bravo%20Team")
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 201, "{copied}");
    assert_eq!(copied["data"][0]["name"], "Bravo Team");

    let (status, _, parented) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Bravo%20Team/parent/Alpha%20Team")
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 200, "{parented}");

    let (status, _, children) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/children")
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 200, "{children}");
    assert_eq!(children["data"][0]["name"], "Bravo Team");

    let (status, _, parent) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Bravo%20Team/parent")
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 200, "{parent}");
    assert_eq!(
        parent["data"]["name"], "Alpha Team",
        "a single object, not an array"
    );

    let (status, _, _) = envelope!(
        app,
        test::TestRequest::delete()
            .uri("/Marti/api/missions/Bravo%20Team/parent")
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 200);

    // Roles, passwords and the expiry, each of which takes a query parameter
    // the route has to parse before the service sees it.
    let (status, _, refused) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/role?clientUid=x&role=MISSION_ADMIN")
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(
        status, 400,
        "an unknown role is a validation failure: {refused}"
    );

    let (status, _, _) = envelope!(
        app,
        test::TestRequest::put()
            .uri(&format!(
                "/Marti/api/missions/guid/{guid}/role?clientUid={OWNER_UID}&role=MISSION_READONLY_SUBSCRIBER"
            ))
            .insert_header(("authorization", bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 200);

    // The role change above demoted the owner, so the rest of this walk is done
    // as an administrator.
    let (_, admin) = server.signed_in("root", true).await;
    let admin_bearer = format!("Bearer {}", admin.token);

    for (method, uri) in [
        (
            "PUT",
            "/Marti/api/missions/Alpha%20Team/password?password=hunter2",
        ),
        ("DELETE", "/Marti/api/missions/Alpha%20Team/password"),
        (
            "PUT",
            "/Marti/api/missions/Alpha%20Team/expiration?expiration=-1",
        ),
    ] {
        let (status, content_type, body) = envelope!(
            app,
            test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .insert_header(("authorization", admin_bearer.clone()))
                .to_request()
        );

        assert_eq!(status, 200, "{method} {uri} answered {status}: {body}");
        assert_eq!(content_type, "application/json");
    }

    // The two administrative listings, and the bare contacts array.
    for uri in [
        "/Marti/api/missions/all/subscriptions",
        "/Marti/api/missions/all/subscriptions/guid",
    ] {
        let (status, _, body) = envelope!(
            app,
            test::TestRequest::get()
                .uri(uri)
                .insert_header(("authorization", admin_bearer.clone()))
                .to_request()
        );

        assert_eq!(status, 200, "{uri} answered {status}: {body}");
        assert!(body["data"].is_array(), "{body}");
    }

    let (status, _, contacts) = envelope!(
        app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/contacts")
            .insert_header(("authorization", admin_bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 200);
    assert!(contacts.is_array(), "contacts is not enveloped: {contacts}");

    // `send` needs a non-empty `contacts`, and tags on a filed item need the
    // item to be filed.
    let (status, _, _) = envelope!(
        app,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Alpha%20Team/send")
            .insert_header(("authorization", admin_bearer.clone()))
            .to_request()
    );

    assert_eq!(status, 400, "send with no contacts is a 400");

    let (status, _, tagged) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/uid/ANDROID-missing/keywords")
            .insert_header(("authorization", admin_bearer.clone()))
            .insert_header((CONTENT_TYPE, "application/json"))
            .set_payload(r#"["tagged"]"#)
            .to_request()
    );

    assert_eq!(
        status, 404,
        "an item that is not filed cannot be tagged: {tagged}"
    );

    // A package body that is not a zip is the one refusal the import has.
    let (status, _, malformed) = envelope!(
        app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Alpha%20Team/contents/missionpackage")
            .insert_header(("authorization", admin_bearer.clone()))
            .set_payload(Bytes::from_static(b"not a zip"))
            .to_request()
    );

    assert_eq!(status, 409, "{malformed}");

    // The archive is M4-02's zip; the route, its read check and its ordering
    // are M4-01's, and a zip body is not enveloped.
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Alpha%20Team/archive")
            .insert_header(("authorization", admin_bearer.clone()))
            .to_request(),
    )
    .await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map(|value| value.to_str().unwrap().to_string())
        .unwrap_or_default();

    assert!(
        status == 200 || status == 501,
        "the archive route is mounted and read-checked: {status}",
    );

    if status == 200 {
        assert_eq!(content_type, "application/zip");
        assert_eq!(
            &test::read_body(response).await[..2],
            b"PK",
            "a mission archive is a zip",
        );
    }
}
