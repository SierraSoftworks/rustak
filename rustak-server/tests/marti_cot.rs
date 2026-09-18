//! `/Marti/api/cot/**` — the CoT query surface, as the two clients read it.
//!
//! R-02 H1: none of these routes existed. Two things depended on them and both
//! failed silently.
//!
//! * **CloudTAK** calls `/cot/xml/{uid}` and `/cot/xml/{uid}/all` through
//!   node-tak's `query.single()` / `query.history()`; a `404` there is a Data
//!   Sync whose markers have no history.
//! * **ATAK** is handed `{public_url}/Marti/api/cot/xml/{uid}` by the oversize
//!   protobuf substitution, so a `404` is a user-visible failed transfer of a
//!   message the server deliberately chose not to send inline.
//!
//! The shapes here are pinned against research `06` §11, which reads `CotApi`
//! line by line: a **single `<event>`** on `/cot/xml/{uid}` and an `<events>`
//! wrapper everywhere else, a bare JSON array from `/cot/matchUid`, and a `404`
//! with an **empty body** rather than the Marti JSON refusal.
//!
//! Run with `cargo test -p rustak-server --features testing --test marti_cot`.

#![cfg(feature = "testing")]

use std::sync::Arc;

use actix_web::http::header::CONTENT_TYPE;
use actix_web::{App, test};
use chrono::{SecondsFormat, Utc};
use rustak_cot::codec::EncodedEvent;
use rustak_cot::detail::{Contact, contact::STREAMING_ENDPOINT};
use rustak_cot::{CotTime, Event};
use rustak_server::cot_store::{CotRecord, latest};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;

/// The channel both the sender and the reader hold.
const CHANNEL: &str = "blue";

/// Stores one relayed message, as the writer task would have.
///
/// The sender's channel bit vector goes with the row, because that is what a
/// read back tomorrow answers "was this reader allowed to see it?" from.
async fn store(server: &TestServer, uid: &str, kind: &str, lat: f64, lon: f64, bits: &[u32]) {
    let event = Event::builder(kind, uid)
        .how("m-g")
        .point(lat, lon)
        .time(CotTime::now())
        .typed(&Contact::new("ALPHA").with_endpoint(STREAMING_ENDPOINT))
        .build();

    let mut groups = GroupSet::new();
    for bitpos in bits {
        groups.set(*bitpos, Direction::In);
    }

    let principal = Principal::new(
        UserId::from(1),
        Username::parse("sender").unwrap(),
        PrincipalKind::Person,
        AuthMethod::SetupToken,
    )
    .with_groups(Arc::new(groups));

    let mut record = CotRecord::new(&EncodedEvent::new(event), &principal, None);
    // The sender is not one of this test server's accounts, and none of these
    // reads is about its row.
    record.user_id = None;
    record.received_at = Utc::now();

    latest::upsert_batch(server.context.db(), vec![record])
        .await
        .expect("store a relayed message");
}

/// The bit position of a channel, creating it if it is not there yet.
async fn channel(server: &TestServer, name: &str) -> u32 {
    let name = GroupName::parse(name).unwrap();
    let db = server.context.db();

    if let Some(row) = db.groups().get_by_name(&name).await.unwrap() {
        return row.bitpos;
    }

    db.groups()
        .create(rustak_server::db::repos::NewGroup::manual(name))
        .await
        .unwrap()
        .bitpos
}

/// An account that receives on `CHANNEL`, and its session token.
async fn reader(server: &TestServer) -> String {
    channel(server, CHANNEL).await;

    let (user, session) = server.signed_in("ada", false).await;
    let db = server.context.db();
    let row = db
        .groups()
        .get_by_name(&GroupName::parse(CHANNEL).unwrap())
        .await
        .unwrap()
        .unwrap();

    db.members()
        .grant(
            user.id,
            row.id,
            Direction::Both,
            rustak_api::MembershipSource::Manual,
        )
        .await
        .unwrap();

    session.token
}

macro_rules! get {
    ($app:expr, $uri:expr, $token:expr) => {{
        let mut request = test::TestRequest::get().uri($uri);

        if let Some(token) = $token {
            request = request.insert_header(("authorization", format!("Bearer {}", token)));
        }

        let response = test::call_service(&$app, request.to_request()).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&test::read_body(response).await).into_owned();

        (status, content_type, body)
    }};
}

#[actix_web::test]
async fn a_single_event_comes_back_as_one_document_with_no_wrapper() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-A", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;
    let (status, content_type, body) = get!(app, "/Marti/api/cot/xml/UID-A", Some(&token));

    assert_eq!(status, 200, "{body}");
    assert_eq!(content_type, "application/xml");
    assert!(
        body.starts_with("<?xml"),
        "upstream is XML_HEADER + the event: {body}",
    );
    assert!(
        !body.contains("<events>"),
        "a single read is one <event>, never a wrapper — node-cot parses it directly: {body}",
    );
    assert_eq!(body.matches("<event ").count(), 1);
    assert!(body.contains("uid=\"UID-A\""));
    assert!(
        !body.contains("<marti"),
        "<marti> says who one delivery was addressed to and is stripped: {body}",
    );
}

#[actix_web::test]
async fn a_uid_nobody_has_heard_of_is_a_404_with_an_empty_body() {
    // Upstream answers a bare 404. Not the Marti JSON refusal shape: node-cot
    // parses this body as XML and ATAK's client treats an unexpected one as a
    // transport failure.
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let (status, _, body) = get!(app, "/Marti/api/cot/xml/UID-NOBODY", Some(&token));

    assert_eq!(status, 404);
    assert!(body.is_empty(), "the body is empty: {body}");
}

#[actix_web::test]
async fn a_message_the_caller_could_not_have_received_answers_the_same_404() {
    // Telling an unauthorised caller that a uid exists is an oracle over
    // everything this server has ever relayed.
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let other = channel(&server, "red").await;
    store(&server, "UID-RED", "a-f-G-U-C", 51.5, -0.12, &[other]).await;

    let app = test::init_service(App::new().configure(server.app())).await;

    let (mine, _, _) = get!(app, "/Marti/api/cot/xml/UID-RED", Some(&token));
    assert_eq!(
        mine, 404,
        "the reader holds no OUT bit that the sender's IN reaches"
    );

    let (missing, _, _) = get!(app, "/Marti/api/cot/xml/UID-ABSENT", Some(&token));
    assert_eq!(
        missing, 404,
        "and it is indistinguishable from one that is not there"
    );
}

#[actix_web::test]
async fn an_anonymous_caller_reads_nothing_at_all() {
    let server = TestServer::start().await;
    let _ = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-A", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;

    let (single, _, _) = get!(app, "/Marti/api/cot/xml/UID-A", None::<&str>);
    assert_eq!(single, 404);

    let (matched, _, body) = get!(app, "/Marti/api/cot/matchUid?search=UID", None::<&str>);
    assert_eq!(matched, 200, "matchUid is always a 200, even empty");
    assert_eq!(body, "[]", "but it holds nothing: {body}");
}

#[actix_web::test]
async fn the_wrapper_routes_answer_one_document_with_one_declaration() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-A", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;
    store(&server, "UID-B", "a-f-G-U-C", 51.6, -0.13, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/cot")
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header((CONTENT_TYPE, "application/json"))
            .set_payload(r#"["UID-A","UID-B"]"#)
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    let body = String::from_utf8_lossy(&test::read_body(response).await).into_owned();

    assert!(body.starts_with("<?xml version='1.0'"), "{body}");
    assert!(body.ends_with("</events>"), "{body}");
    assert_eq!(
        body.matches("<?xml").count(),
        1,
        "a stored row is a whole document; the wrapper holds elements, so the \
         declaration comes off each one — otherwise nothing can parse this: {body}",
    );
    assert_eq!(body.matches("<event ").count(), 2, "{body}");
}

#[actix_web::test]
async fn an_empty_uid_list_is_a_400() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for body in ["[]", "", "not json"] {
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/api/cot")
                .insert_header(("authorization", format!("Bearer {token}")))
                .insert_header((CONTENT_TYPE, "application/json"))
                .set_payload(body)
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status().as_u16(),
            400,
            "upstream throws IllegalArgumentException for {body:?}",
        );
    }
}

#[actix_web::test]
async fn situational_awareness_needs_a_window_and_refuses_one_over_a_day() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-A", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;
    let start =
        (Utc::now() - chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let end =
        (Utc::now() + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let long_ago =
        (Utc::now() - chrono::Duration::days(3)).to_rfc3339_opts(SecondsFormat::Millis, true);

    let (missing, _, _) = get!(app, "/Marti/api/cot/sa", Some(&token));
    assert_eq!(missing, 400, "start and end are both required");

    let (wide, _, _) = get!(
        app,
        &format!("/Marti/api/cot/sa?start={long_ago}&end={end}"),
        Some(&token)
    );
    assert_eq!(wide, 400, "upstream throws for a window over 24 hours");

    let (ok, content_type, body) = get!(
        app,
        &format!("/Marti/api/cot/sa?start={start}&end={end}"),
        Some(&token)
    );
    assert_eq!(ok, 200, "{body}");
    assert_eq!(content_type, "application/xml");
    assert!(body.contains("uid=\"UID-A\""), "{body}");
}

#[actix_web::test]
async fn a_bounding_box_narrows_the_answer_and_half_a_box_is_refused() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-LONDON", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;
    store(&server, "UID-CAPETOWN", "a-f-G-U-C", -33.9, 18.4, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;
    let start =
        (Utc::now() - chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let end =
        (Utc::now() + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);

    let (status, _, body) = get!(
        app,
        &format!("/Marti/api/cot/sa?start={start}&end={end}&left=-1&bottom=50&right=1&top=52"),
        Some(&token)
    );

    assert_eq!(status, 200, "{body}");
    assert!(body.contains("UID-LONDON"), "{body}");
    assert!(!body.contains("UID-CAPETOWN"), "{body}");

    let (partial, _, _) = get!(
        app,
        &format!("/Marti/api/cot/sa?start={start}&end={end}&left=-1&bottom=50"),
        Some(&token)
    );
    assert_eq!(
        partial, 400,
        "half a box would silently answer for the whole world",
    );

    // And a box that holds nothing is the empty 404, not an empty wrapper.
    let (empty, _, empty_body) = get!(
        app,
        &format!("/Marti/api/cot/sa?start={start}&end={end}&left=100&bottom=80&right=101&top=81"),
        Some(&token)
    );
    assert_eq!(empty, 404);
    assert!(empty_body.is_empty(), "{empty_body}");
}

#[actix_web::test]
async fn match_uid_is_a_bare_array_with_no_envelope() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-ALPHA", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;
    store(&server, "UID-BRAVO", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;

    let (status, content_type, body) =
        get!(app, "/Marti/api/cot/matchUid?search=ALPHA", Some(&token));

    assert_eq!(status, 200);
    assert_eq!(content_type, "application/json");
    assert_eq!(
        body, r#"["UID-ALPHA"]"#,
        "a bare List<String>, never the Marti envelope: {body}",
    );

    let (nothing, _, empty) = get!(
        app,
        "/Marti/api/cot/matchUid?search=nothing-like-this",
        Some(&token)
    );
    assert_eq!(nothing, 200, "always a 200, even when nothing matched");
    assert_eq!(empty, "[]");
}

#[actix_web::test]
async fn a_history_read_is_a_wrapper_and_an_unheard_of_uid_is_the_empty_404() {
    let server = TestServer::start().await;
    let token = reader(&server).await;
    let bitpos = channel(&server, CHANNEL).await;
    store(&server, "UID-A", "a-f-G-U-C", 51.5, -0.12, &[bitpos]).await;

    let app = test::init_service(App::new().configure(server.app())).await;

    // No history segments were written here, so the window holds nothing — the
    // empty 404 rather than an empty wrapper, which is upstream's shape.
    let (status, _, body) = get!(
        app,
        "/Marti/api/cot/xml/UID-A/all?secago=3600",
        Some(&token)
    );
    assert_eq!(status, 404, "{body}");
    assert!(body.is_empty());

    let (unheard, _, _) = get!(
        app,
        "/Marti/api/cot/xml/UID-NOBODY/all?secago=3600",
        Some(&token)
    );
    assert_eq!(unheard, 404);
}

#[actix_web::test]
async fn a_wrong_verb_on_a_cot_route_is_a_405_rather_than_a_404() {
    // `marti::PATHS` is the one table that says what is mounted; a route added
    // without its entry answers 404 where it means 405 and confuses a client
    // into trying another spelling.
    let server = TestServer::start().await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for uri in [
        "/Marti/api/cot",
        "/Marti/api/cot/sa",
        "/Marti/api/cot/matchUid",
        "/Marti/api/cot/xml/UID-A",
        "/Marti/api/cot/xml/UID-A/all",
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(actix_web::http::Method::PATCH)
                .uri(uri)
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status().as_u16(),
            405,
            "{uri} is mounted, so a verb it does not serve is a 405",
        );
    }
}
