//! Invitations, log entries, layers and the archive, end to end.
//!
//! The half of Data Sync that hangs off a mission rather than being one.
//! `missions_flow.rs` covers create, subscribe, contents and delete; this suite
//! covers what an operator does afterwards.
//!
//! # What breaks if these fail
//!
//! * **`GET /missions/invitations?clientUid=` answering anything but `200`.**
//!   CloudTAK fetches it in parallel with the mission list and treats a failure
//!   as a failure of the whole page, so a caller with no invitations has to get
//!   an empty array rather than a `404`.
//! * **A log write answering `200`.** Both the create and the update answer
//!   `201`; a client that checks the status treats anything else as a failure
//!   and writes the entry again.
//! * **`mission_layers`.** The one deliberately snake_case key in the whole
//!   mission API. A camelCase spelling is a layer tree no client can read.
//! * **The archive round trip.** The archive is what a delete stores as its
//!   undo, so an archive that cannot be imported back is a delete that cannot
//!   be reversed.
//!
//! Run with `cargo test -p rustak-server --features testing --test missions_extras`.

#![cfg(feature = "testing")]

use actix_web::body::MessageBody as _;
use actix_web::{App, test};
use serde_json::{Value, json};

use rustak_server::testing::TestServer;

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// The device that owns every mission here.
const OWNER_UID: &str = "ANDROID-owner";

/// The device that is invited to them.
const GUEST_UID: &str = "ANDROID-guest";

/// Signs in, creates a mission and answers the session token and its guid.
async fn mission(server: &TestServer, name: &str) -> (String, String) {
    let (_, session) = server.signed_in("grace", false).await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri(&format!(
                "/Marti/api/missions/{name}?creatorUid={OWNER_UID}"
            ))
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 201);

    let body: Value = test::read_body_json(response).await;
    let guid = body["data"][0]["guid"]
        .as_str()
        .expect("a guid")
        .to_string();

    (session.token, guid)
}

/// Calls the API with a session, returning the status and the parsed body.
macro_rules! call {
    ($server:expr, $token:expr, $request:expr) => {{
        let app = app!($server);
        let response = test::call_service(
            &app,
            $request
                .insert_header(("authorization", format!("Bearer {}", $token)))
                .to_request(),
        )
        .await;
        let status = response.status().as_u16();
        let body: Value = test::read_body_json(response).await;

        (status, body)
    }};
}

#[actix_web::test]
async fn an_invitation_is_listed_for_the_device_it_names_and_carries_its_token() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri(&format!(
            "/Marti/api/missions/Kettle/invite/clientUid/{GUEST_UID}?creatorUid={OWNER_UID}"
        ))
    );
    assert_eq!(status, 200, "the invitation is written");

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri(&format!(
            "/Marti/api/missions/invitations?clientUid={GUEST_UID}"
        ))
    );

    assert_eq!(status, 200);
    assert_eq!(body["type"], "MissionInvitation");
    let listed = body["data"].as_array().expect("an array");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["missionName"], "Kettle");
    assert_eq!(listed[0]["type"], "clientUid");
    assert!(
        listed[0]["token"].is_string(),
        "the invitation carries the token that makes it actionable",
    );
}

#[actix_web::test]
async fn a_device_with_no_invitations_gets_an_empty_array_not_a_404() {
    // CloudTAK fetches this in parallel with the mission list and fails the
    // whole page on anything that is not a 200.
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/invitations?clientUid=ANDROID-nobody")
    );

    assert_eq!(status, 200);
    assert_eq!(body["data"], json!([]));
}

#[actix_web::test]
async fn an_invitation_lets_a_device_subscribe_to_an_invite_only_mission() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri("/Marti/api/missions/Kettle?inviteOnly=true")
    );
    assert_eq!(status, 200);

    let (refused, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri(&format!(
            "/Marti/api/missions/Kettle/subscription?uid={GUEST_UID}"
        ))
    );
    assert_eq!(refused, 403, "an invite-only mission refuses a stranger");

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri(&format!(
            "/Marti/api/missions/Kettle/invite/clientUid/{GUEST_UID}?creatorUid={OWNER_UID}"
        ))
    );
    assert_eq!(status, 200);

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::put().uri(&format!(
            "/Marti/api/missions/Kettle/subscription?uid={GUEST_UID}"
        ))
    );

    assert_eq!(status, 201, "the invitation let it in: {body}");
    assert!(body["data"]["token"].is_string());

    // Spent: a subscriber that is later removed must not re-join on the
    // strength of the same invitation.
    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri(&format!(
            "/Marti/api/missions/invitations?clientUid={GUEST_UID}"
        ))
    );
    assert_eq!(status, 200);
    assert_eq!(body["data"], json!([]), "the invitation was spent");
}

#[actix_web::test]
async fn a_log_entry_answers_201_and_names_every_mission_it_was_written_to() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::post()
            .uri("/Marti/api/missions/logs/entries")
            .set_json(json!({
                "content": "first light",
                "creatorUid": OWNER_UID,
                "missionNames": ["Kettle"],
                "keywords": ["sitrep"],
            }))
    );

    assert_eq!(status, 201, "both log writes answer 201: {body}");
    assert_eq!(body["type"], "com.bbn.marti.sync.model.LogEntry");
    let id = body["data"]["id"].as_str().expect("an id").to_string();
    assert_eq!(body["data"]["missionNames"], json!(["Kettle"]));
    assert!(
        body["data"]["servertime"].is_string(),
        "lower-case t, unlike MissionChange.serverTime",
    );

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri(&format!("/Marti/api/missions/logs/entries/{id}"))
    );
    assert_eq!(status, 200);
    assert_eq!(body["data"][0]["content"], "first light");

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/Kettle/log")
    );
    assert_eq!(status, 200);

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::delete().uri(&format!("/Marti/api/missions/logs/entries/{id}"))
    );
    assert_eq!(status, 200);
}

#[actix_web::test]
async fn a_log_write_refuses_the_field_the_server_owns() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::post()
            .uri("/Marti/api/missions/logs/entries")
            .set_json(json!({ "id": "mine", "content": "x", "missionNames": ["Kettle"] }))
    );
    assert_eq!(status, 400, "a POST must not carry an id");

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/logs/entries")
            .set_json(json!({ "content": "x", "missionNames": ["Kettle"] }))
    );
    assert_eq!(status, 400, "a PUT has to carry one");

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/logs/entries")
            .set_json(json!({
                "id": "mine",
                "content": "x",
                "servertime": "2026-09-17T12:00:40.000Z",
                "missionNames": ["Kettle"],
            }))
    );
    assert_eq!(status, 400, "servertime is assigned by the server");
}

#[actix_web::test]
async fn the_layer_tree_uses_the_one_snake_case_key_in_the_api() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Kettle/layers?name=Markers&type=GROUP&uid=layer-root")
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["type"], "MissionLayer");

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri(
            "/Marti/api/missions/Kettle/layers?name=Inner&type=UID&uid=layer-child\
             &parentUid=layer-root"
        )
    );
    assert_eq!(status, 200);

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/Kettle/layers")
    );

    assert_eq!(status, 200);
    let roots = body["data"].as_array().expect("an array");
    assert_eq!(roots.len(), 1, "only the root is at the top level");
    assert_eq!(roots[0]["uid"], "layer-root");
    assert_eq!(
        roots[0]["mission_layers"][0]["uid"], "layer-child",
        "mission_layers is snake_case on purpose: {body}",
    );
}

#[actix_web::test]
async fn deleting_a_layer_unfiles_what_was_in_it_rather_than_removing_it() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Kettle/layers?name=Markers&type=UID&uid=layer-root")
    );
    assert_eq!(status, 200);

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Kettle/contents")
            .set_json(json!({ "paths": { "layer-root": [{ "uids": ["UID-MARKER"] }] } }))
    );
    assert_eq!(status, 200);

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::delete().uri("/Marti/api/missions/Kettle/layers?uid=layer-root")
    );
    assert_eq!(status, 200);

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/Kettle")
    );

    assert_eq!(status, 200);
    let uids = body["data"][0]["uids"].as_array().expect("an array");
    assert_eq!(uids.len(), 1, "the item is still in the mission: {body}");
    assert_eq!(uids[0]["data"], "UID-MARKER");
}

#[actix_web::test]
async fn a_map_layer_comes_back_with_the_field_we_have_never_heard_of() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Kettle/maplayers")
            .set_json(json!({ "name": "OSM", "somethingOurs": 42 }))
    );

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["type"], "MapLayer");
    assert_eq!(body["data"]["somethingOurs"], 42);

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/Kettle")
    );

    assert_eq!(status, 200);
    assert_eq!(
        body["data"][0]["mapLayers"].as_array().map(Vec::len),
        Some(1),
    );
}

#[actix_web::test]
async fn external_data_is_reported_on_the_mission_that_carries_it() {
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::post()
            .uri("/Marti/api/missions/Kettle/externaldata")
            .set_json(json!({ "name": "Weather", "tool": "wx" }))
    );

    assert_eq!(status, 201, "{body}");
    assert_eq!(body["type"], "ExternalMissionData");

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/Kettle")
    );

    assert_eq!(status, 200);
    assert_eq!(body["data"][0]["externalData"][0]["name"], "Weather");
}

#[actix_web::test]
async fn an_archive_is_a_zip_that_imports_back_into_a_new_mission() {
    // The archive is what a delete stores as its undo, so it has to go back in
    // through the ordinary package import with nothing archive-specific.
    let server = TestServer::start().await;
    let (token, _) = mission(&server, "Kettle").await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Kettle/contents")
            .set_json(json!({ "uids": ["UID-MARKER"] }))
    );
    assert_eq!(status, 200);

    let app = app!(&server);
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/missions/Kettle/archive")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/zip"),
    );
    let disposition = response
        .headers()
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        disposition.starts_with("attachment; filename=\""),
        "the filename is quoted: {disposition}",
    );

    let zip = response.into_body().try_into_bytes().expect("the archive");
    assert!(!zip.is_empty());

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::post().uri("/Marti/api/missions/Anvil?creatorUid=ANDROID-owner")
    );
    assert_eq!(status, 201);

    let app = app!(&server);
    let response = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Anvil/contents/missionpackage?creatorUid=ANDROID-owner")
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header(("content-type", "application/zip"))
            .set_payload(zip.clone())
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200, "the archive imports back");

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::get().uri("/Marti/api/missions/Anvil")
    );

    assert_eq!(status, 200);
    let uids = body["data"][0]["uids"].as_array().expect("an array");
    assert_eq!(
        uids.iter()
            .filter_map(|added| added["data"].as_str())
            .collect::<Vec<_>>(),
        vec!["UID-MARKER"],
        "the same uid came back: {body}",
    );
}

#[actix_web::test]
async fn setting_a_role_and_removing_a_subscription_are_administrative() {
    let server = TestServer::start().await;
    let (token, guid) = mission(&server, "Kettle").await;
    let (_, admin) = server.signed_in("root", true).await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri(&format!(
            "/Marti/api/missions/Kettle/subscription?uid={GUEST_UID}"
        ))
    );
    assert_eq!(status, 201);

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri(&format!(
                "/api/v1/missions/{guid}/subscriptions/{GUEST_UID}/role"
            ))
            .set_json(json!({ "role": "MISSION_READONLY_SUBSCRIBER" }))
    );
    assert_eq!(status, 403, "an ordinary session may not administer");

    let (status, body) = call!(
        &server,
        admin.token,
        test::TestRequest::put()
            .uri(&format!(
                "/api/v1/missions/{guid}/subscriptions/{GUEST_UID}/role"
            ))
            .set_json(json!({ "role": "MISSION_READONLY_SUBSCRIBER" }))
    );
    assert_eq!(status, 200, "{body}");

    let app = app!(&server);
    let response = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&format!(
                "/api/v1/missions/{guid}/subscriptions/{GUEST_UID}"
            ))
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 204);
}

#[actix_web::test]
async fn the_admin_listing_shows_a_mission_a_client_would_never_be_told_about() {
    let server = TestServer::start().await;
    let (token, guid) = mission(&server, "Kettle").await;
    let (_, admin) = server.signed_in("root", true).await;

    let (status, _) = call!(
        &server,
        token,
        test::TestRequest::put().uri("/Marti/api/missions/Kettle?inviteOnly=true")
    );
    assert_eq!(status, 200);

    let (status, listed) = call!(
        &server,
        admin.token,
        test::TestRequest::get().uri("/api/v1/missions")
    );

    assert_eq!(status, 200);
    let rows = listed.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "invite-only is still listed here: {listed}");
    assert_eq!(rows[0]["invite_only"], true);
    assert_eq!(rows[0]["guid"], guid);

    let (status, detail) = call!(
        &server,
        admin.token,
        test::TestRequest::get().uri(&format!("/api/v1/missions/{guid}"))
    );

    assert_eq!(status, 200);
    assert_eq!(detail["name"], "Kettle", "the summary is flattened");
    assert!(detail["subscriptions"].is_array());
    assert!(detail["layers"].is_array());
}

/// The status of a request whose answer may have no body at all.
macro_rules! status_of {
    ($server:expr, $token:expr, $request:expr) => {{
        let app = app!($server);
        let response = test::call_service(
            &app,
            $request
                .insert_header(("authorization", format!("Bearer {}", $token)))
                .to_request(),
        )
        .await;

        response.status().as_u16()
    }};
}

#[actix_web::test]
async fn a_deleted_mission_is_listed_only_when_it_is_asked_for_and_answers_410_with_itself() {
    // The row is kept so a client syncing late can be told the mission went.
    // Until this, nothing could show an operator that it had — which is most of
    // the value of keeping it.
    let server = TestServer::start().await;
    let (token, guid) = mission(&server, "Standdown").await;
    let (_, admin) = server.signed_in("root", true).await;

    let status = status_of!(
        &server,
        admin.token,
        test::TestRequest::delete().uri(&format!("/api/v1/missions/{guid}"))
    );
    assert_eq!(status, 204);

    let (status, listed) = call!(
        &server,
        admin.token,
        test::TestRequest::get().uri("/api/v1/missions")
    );
    assert_eq!(status, 200);
    assert!(
        listed.as_array().expect("an array").is_empty(),
        "the default listing is still what is live: {listed}",
    );

    let (status, listed) = call!(
        &server,
        admin.token,
        test::TestRequest::get().uri("/api/v1/missions?include_deleted=true")
    );
    assert_eq!(status, 200);
    let rows = listed.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["guid"], guid);
    assert!(
        rows[0]["deleted_at"].is_string(),
        "the row says when it went: {listed}",
    );

    // The detail is a `410` — it is not here — carrying the mission, because
    // "what was this and when did it go" is the question being asked.
    let (status, detail) = call!(
        &server,
        admin.token,
        test::TestRequest::get().uri(&format!("/api/v1/missions/{guid}"))
    );

    assert_eq!(status, 410);
    assert_eq!(detail["name"], "Standdown", "{detail}");
    assert!(detail["deleted_at"].is_string(), "{detail}");
    assert!(detail["layers"].is_array());

    // Deleting it again says the same thing the detail's status does, rather
    // than a `404` (it is here) or a `500` (it was, before this brief).
    let status = status_of!(
        &server,
        admin.token,
        test::TestRequest::delete().uri(&format!("/api/v1/missions/{guid}"))
    );
    assert_eq!(status, 410, "deleting it twice is gone, not not-found");

    let status = status_of!(
        &server,
        token,
        test::TestRequest::get().uri("/api/v1/missions?include_deleted=true")
    );
    assert_eq!(status, 403, "the listing is administrative either way");
}

#[actix_web::test]
async fn a_layer_reports_how_many_items_are_actually_filed_under_it() {
    // It reported zero for every layer before this, while the tree drew the
    // number — so a folder with four markers in it said it was empty.
    let server = TestServer::start().await;
    let (token, guid) = mission(&server, "Anvil").await;
    let (_, admin) = server.signed_in("root", true).await;

    for (name, uid) in [("Markers", "layer-root"), ("Empty", "layer-empty")] {
        let (status, body) = call!(
            &server,
            token,
            test::TestRequest::put().uri(&format!(
                "/Marti/api/missions/Anvil/layers?name={name}&type=UID&uid={uid}"
            ))
        );
        assert_eq!(status, 200, "{body}");
    }

    let (status, body) = call!(
        &server,
        token,
        test::TestRequest::put()
            .uri("/Marti/api/missions/Anvil/contents")
            .set_json(json!({
                "paths": { "layer-root": [{ "uids": ["UID-ONE", "UID-TWO"] }] }
            }))
    );
    assert_eq!(status, 200, "{body}");

    let (status, detail) = call!(
        &server,
        admin.token,
        test::TestRequest::get().uri(&format!("/api/v1/missions/{guid}"))
    );
    assert_eq!(status, 200);

    let layers = detail["layers"].as_array().expect("an array");
    let count_of = |uid: &str| {
        layers
            .iter()
            .find(|layer| layer["uid"] == uid)
            .and_then(|layer| layer["item_count"].as_i64())
            .unwrap_or(-1)
    };

    assert_eq!(count_of("layer-root"), 2, "{detail}");
    assert_eq!(
        count_of("layer-empty"),
        0,
        "a layer with nothing in it still says zero: {detail}",
    );
}
