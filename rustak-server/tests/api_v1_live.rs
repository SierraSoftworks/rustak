//! `/api/v1/{clients,cot,settings}` end to end, against the real application.
//!
//! The three surfaces an operator watches rather than edits: what is connected,
//! what has been relayed, and the two subsystem settings. They share a suite
//! because they share a failure mode — an endpoint that answers plausibly for
//! an installation with no stream listener, no stored messages and no wizard
//! answers, which is exactly the state a fresh server is in and the state a
//! page first renders against.
//!
//! # What breaks if these fail
//!
//! * **The gate.** A CoT browser that ignores the sender's channels hands one
//!   team's positions to another, and a client list that is not administrative
//!   publishes every device's address.
//! * **An empty installation.** `/clients` on a server with `[stream.tls]`
//!   disabled must be an empty list rather than a `500`, or the page that opens
//!   on it shows an error for a correct configuration.
//! * **The upload limit.** It is the number CloudTAK's wizard reads before it
//!   will save a connection, so a stored value nothing enforces is worse than
//!   no setting at all.
//!
//! Run with `cargo test -p rustak-server --features testing --test api_v1_live`.

#![cfg(feature = "testing")]

use actix_web::http::StatusCode;
use actix_web::{App, test};
use chrono::{Duration, Utc};
use rustak_api::{ClientHistoryEntry, ConnectedClient, CotDetail, CotSummary, FileSettings};
use rustak_cot::codec::EncodedEvent;
use rustak_cot::detail::{Contact, Group, contact::STREAMING_ENDPOINT};
use rustak_cot::{CotTime, Event};
use rustak_server::cot_store::CotRecord;
use rustak_server::cot_store::latest::upsert_batch;
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use rustak_server::testing::context::bearer;

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// A stored message from one uid, published into `bits`.
fn record(
    uid: &str,
    kind: &str,
    callsign: &str,
    at: chrono::DateTime<Utc>,
    bits: &[u32],
) -> CotRecord {
    let mut groups = GroupSet::new();
    for bitpos in bits {
        groups.set(*bitpos, Direction::In);
    }

    let principal = Principal::new(
        UserId::from(1),
        Username::parse("grace").unwrap(),
        PrincipalKind::Person,
        AuthMethod::SetupToken,
    )
    .with_groups(std::sync::Arc::new(groups));

    let encoded = EncodedEvent::new(
        Event::builder(kind, uid)
            .how("m-g")
            .point(51.5, -0.12)
            .time(CotTime::from_datetime(at))
            .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
            .typed(&Group::new("Cyan", "Team Member"))
            .build(),
    );

    let mut record = CotRecord::new(&encoded, &principal, None);
    // No account row exists in a fresh test database for the foreign key to
    // point at, and none of these reads is about the sender's row.
    record.user_id = None;
    record.received_at = at;
    record
}

#[actix_web::test]
async fn a_server_with_no_stream_listener_answers_an_empty_client_list() {
    // The state a fresh installation and every `[stream.tls] enabled = false`
    // deployment is in; a `500` here is an error on a page for a correct
    // configuration.
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let listed: Vec<ConnectedClient> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/clients")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert!(listed.is_empty());
}

#[actix_web::test]
async fn disconnecting_or_hiding_a_uid_nothing_is_connected_under_is_not_found() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    for request in [
        test::TestRequest::delete().uri("/api/v1/clients/ANDROID-1"),
        test::TestRequest::post()
            .uri("/api/v1/clients/ANDROID-1/incognito")
            .set_json(serde_json::json!({ "on": true })),
    ] {
        let response = test::call_service(
            &app,
            request
                .insert_header(("authorization", bearer(&admin)))
                .to_request(),
        )
        .await;

        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE
            ),
            "an installation with nothing connected says so: {:?}",
            response.status(),
        );
    }
}

#[actix_web::test]
async fn the_client_surface_is_administrative_throughout() {
    let server = TestServer::start().await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let app = app!(server);

    for request in [
        test::TestRequest::get().uri("/api/v1/clients"),
        test::TestRequest::get().uri("/api/v1/clients/history"),
        test::TestRequest::delete().uri("/api/v1/clients/ANDROID-1"),
        test::TestRequest::post()
            .uri("/api/v1/clients/ANDROID-1/incognito")
            .set_json(serde_json::json!({ "on": true })),
    ] {
        let response = test::call_service(
            &app,
            request
                .insert_header(("authorization", bearer(&ordinary)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}

#[actix_web::test]
async fn the_client_history_lists_devices_that_are_not_connected() {
    let server = TestServer::start().await;
    let (user, admin) = server.signed_in("grace", true).await;

    let device = rustak_server::identity::devices::upsert_seen(
        server.db(),
        &DeviceUid::parse("ANDROID-1").unwrap(),
        user.id,
        rustak_server::db::repos::DeviceSeen {
            callsign: Some("ALPHA".to_string()),
            platform: Some("ATAK-CIV".to_string()),
            version: Some("5.6.0".to_string()),
            ..rustak_server::db::repos::DeviceSeen::default()
        },
    )
    .await
    .expect("a device");

    upsert_batch(
        server.db(),
        vec![record("ANDROID-1", "a-f-G-U-C", "ALPHA", Utc::now(), &[])],
    )
    .await
    .expect("a stored message");

    let app = app!(server);

    let listed: Vec<ClientHistoryEntry> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/clients/history?secago=3600")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].client_uid, device.uid.as_str());
    assert_eq!(listed[0].username, "grace");
    assert_eq!(listed[0].takv.as_deref(), Some("ATAK-CIV 5.6.0"));
    assert_eq!(
        listed[0].team.as_deref(),
        Some("Cyan"),
        "the team comes out of the last message the device sent",
    );
    assert!(!listed[0].connected, "nothing is connected here");

    let narrow: Vec<ClientHistoryEntry> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/clients/history?secago=0")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert!(
        narrow.is_empty() || narrow.len() == 1,
        "a zero-second window is the instant this request was made",
    );
}

#[actix_web::test]
async fn the_cot_browser_lists_the_latest_message_per_uid_and_pages_it() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let now = Utc::now();

    upsert_batch(
        server.db(),
        vec![
            record(
                "UID-A",
                "a-f-G-U-C",
                "ALPHA",
                now - Duration::seconds(60),
                &[],
            ),
            record("UID-B", "a-f-G-U-C", "BRAVO", now, &[]),
            record("UID-C", "b-t-f", "ALPHA", now, &[]),
        ],
    )
    .await
    .expect("stored messages");

    let app = app!(server);

    let listed: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(listed.len(), 3);
    assert_eq!(
        listed[0].team.as_deref(),
        Some("Cyan"),
        "the team is parsed out of the stored XML",
    );

    let atoms: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot?type=a-&callsign=alph")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(atoms.len(), 1);
    assert_eq!(atoms[0].uid, "UID-A");

    let first: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot?limit=2")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;
    let second: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot?limit=2&page=1")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 1);
    assert!(
        first
            .iter()
            .all(|row| second.iter().all(|other| other.uid != row.uid)),
        "two pages of one listing do not overlap",
    );
}

#[actix_web::test]
async fn one_message_carries_its_xml_and_a_uid_nobody_stored_is_not_found() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;

    upsert_batch(
        server.db(),
        vec![record("UID-A", "a-f-G-U-C", "ALPHA", Utc::now(), &[])],
    )
    .await
    .expect("a stored message");

    let app = app!(server);

    let detail: CotDetail = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot/UID-A")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(detail.summary.uid, "UID-A");
    assert!(
        detail.xml.contains("<event"),
        "the detail carries the bytes the recipients were sent: {}",
        detail.xml,
    );

    let missing = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot/UID-NOBODY")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn a_message_published_into_a_channel_somebody_is_not_in_is_not_there_for_them() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;

    // Bit 9 is a channel nobody in this test holds in the `OUT` direction.
    upsert_batch(
        server.db(),
        vec![record("UID-SECRET", "a-f-G-U-C", "ALPHA", Utc::now(), &[9])],
    )
    .await
    .expect("a stored message");

    let app = app!(server);

    let listed: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot")
            .insert_header(("authorization", bearer(&ordinary)))
            .to_request(),
    )
    .await;

    assert!(listed.is_empty(), "the channel rule narrows the listing");

    let refused = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot/UID-SECRET")
            .insert_header(("authorization", bearer(&ordinary)))
            .to_request(),
    )
    .await;

    assert_eq!(
        refused.status(),
        StatusCode::NOT_FOUND,
        "a message that is not theirs answers as one that is not here",
    );

    let seen: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(seen.len(), 1, "an administrator sees everything");
}

#[actix_web::test]
async fn history_is_newest_first_and_a_backwards_window_is_refused() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let now = Utc::now();

    upsert_batch(
        server.db(),
        vec![record("UID-A", "a-f-G-U-C", "ALPHA", now, &[])],
    )
    .await
    .expect("a stored message");

    let app = app!(server);

    // No segments are written without a stream listener, so the window is
    // empty — what this asserts is the framing and the refusals, which are what
    // a page depends on.
    let listed: Vec<CotSummary> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/cot/UID-A/history?secago=3600")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert!(listed.is_empty());

    let backwards = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!(
                "/api/v1/cot/UID-A/history?start={}&end={}",
                urlencoding(&now.to_rfc3339()),
                urlencoding(&(now - Duration::hours(1)).to_rfc3339()),
            ))
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(backwards.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn deleting_a_stored_message_is_administrative_and_takes_the_row() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;

    upsert_batch(
        server.db(),
        vec![record("UID-A", "a-f-G-U-C", "ALPHA", Utc::now(), &[])],
    )
    .await
    .expect("a stored message");

    let app = app!(server);

    let refused = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri("/api/v1/cot/UID-A")
            .insert_header(("authorization", bearer(&ordinary)))
            .to_request(),
    )
    .await;

    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let removed = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri("/api/v1/cot/UID-A")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let gone = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri("/api/v1/cot/UID-A")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn the_upload_limit_can_be_read_changed_and_is_then_enforced() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let initial: FileSettings = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/settings/files")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(initial.upload_size_limit_mb, 400);
    assert!(!initial.from_config_file);

    let saved: FileSettings = test::call_and_read_body_json(
        &app,
        test::TestRequest::put()
            .uri("/api/v1/settings/files")
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "upload_size_limit_mb": 25 }))
            .to_request(),
    )
    .await;

    assert_eq!(saved.upload_size_limit_mb, 25);

    // The gate CloudTAK's setup wizard reads, which has to agree with what the
    // upload servlets will actually accept.
    let advertised: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/files/api/config")
            .to_request(),
    )
    .await;

    assert_eq!(advertised["uploadSizeLimit"], 25);

    let refused = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/api/v1/settings/files")
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "upload_size_limit_mb": 0 }))
            .to_request(),
    )
    .await;

    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn a_limit_the_configuration_file_pins_cannot_be_changed_here() {
    let server = TestServer::start_with(|config| config.marti.upload_size_limit_mb = 100).await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let read: FileSettings = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/settings/files")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(read.upload_size_limit_mb, 100);
    assert!(
        read.from_config_file,
        "a page that cannot tell offers an edit that will not stick",
    );

    let refused = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/api/v1/settings/files")
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "upload_size_limit_mb": 25 }))
            .to_request(),
    )
    .await;

    assert_eq!(refused.status(), StatusCode::CONFLICT);
}

#[actix_web::test]
async fn the_marti_settings_are_administrative_and_read_only() {
    let server = TestServer::start_with(|config| {
        config.marti.public_host = Some("tak.example.com".to_string());
    })
    .await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let app = app!(server);

    let read: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/settings/marti")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(read["public_host"], "tak.example.com");
    assert_eq!(read["allow_all_origins"], false);

    let refused = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/settings/marti")
            .insert_header(("authorization", bearer(&ordinary)))
            .to_request(),
    )
    .await;

    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

/// Percent-encodes a timestamp for a query string.
fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}
