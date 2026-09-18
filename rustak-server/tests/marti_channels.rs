//! Channels, contacts and client endpoints, over a real stream listener.
//!
//! The unit tests in `src/marti/**` check the shapes. This suite checks the
//! thing the shapes are for: an HTTP call on one side changes what a device
//! connected on the other side receives, and the notice that tells it to go and
//! re-read its channels reaches the right devices and not the wrong ones.
//!
//! Everything goes through the real thing — a real certificate authority, a
//! real enrolment, a real mutually authenticated handshake and
//! `rustak_client::stream::testing::Eud` on the far end — driven by the real
//! `App` from `web::server::services`. The one seam is
//! [`AppContext::install_live`], which the runtime performs when the listener
//! binds and this suite performs by hand because it binds the listener itself.

#![cfg(feature = "testing")]

mod stream_support;

use std::sync::Arc;

use actix_web::{App, test};
use rustak_api::identity::{Direction, Username};
use rustak_client::stream::testing::Eud;
use rustak_cot::types::cot_type;
use rustak_server::auth::{JwtIssuer, RateLimiter};
use rustak_server::prelude::Services as _;

use stream_support::{EXPECT, Harness, SETTLE};

const BOTH: Direction = Direction::Both;

/// Reads until nothing more arrives.
///
/// Not `stream_support::settle`, which asserts that nothing arrives at all:
/// every test here has clients that have already exchanged position reports, so
/// what has to be true is that the connection is *quiet* before the assertion,
/// not that it always was.
async fn drain(eud: &mut Eud) {
    while eud.expect(|_| true, SETTLE).await.is_ok() {}
}

/// Waits until the listener has let go of everything but `remaining`.
async fn await_only(harness: &Harness, remaining: usize) {
    for _ in 0..200 {
        if harness.live.connected() <= remaining {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("still {} connected", harness.live.connected());
}

/// Installs the handles the runtime would have, and signs an account in.
///
/// `install_jwt` and `install_live` are once-per-process on a context, so the
/// result of a second call is deliberately dropped: a test that signs two
/// accounts in wants the second token, not a failure about the first install.
async fn sign_in(harness: &Harness, username: &str) -> String {
    let context = &harness.context;

    let issuer = JwtIssuer::load_or_create(
        context.db(),
        context.secrets(),
        &context.config().auth,
        "https://localhost",
    )
    .await
    .expect("token signing keys");

    let _ = context.install_jwt(Arc::new(issuer));
    let _ = context.install_live(Arc::new(harness.live.clone()));

    let user = context
        .db()
        .users()
        .get_by_username(&Username::parse(username).expect("a usable username"))
        .await
        .expect("the account under test")
        .expect("the account was enrolled first");

    let session = rustak_server::testing::session_for(context, &user, false).await;

    format!("Bearer {}", session.token)
}

/// The routes the public listener serves, over the harness's own context.
fn routes(harness: &Harness) -> impl FnOnce(&mut actix_web::web::ServiceConfig) + Clone {
    let limiter = Arc::new(RateLimiter::new(&harness.context.config().auth.rate_limit));

    rustak_server::web::server::services(harness.context.clone(), limiter)
}

#[tokio::test]
async fn the_listing_carries_everything_ataks_parser_refuses_to_do_without() {
    // `compat/groups.md` §1: ATAK drops a channel with no `bitpos`, no
    // `created`, no `type` or no `direction`, and a whole response whose
    // `created` will not parse as `yyyy-MM-dd`.
    let harness = Harness::start().await;
    harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/groups/all?useCache=true")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(body["type"], "com.bbn.marti.remote.groups.Group");
    assert_eq!(body["version"], "3");

    let data = body["data"].as_array().expect("an array of channels");
    assert_eq!(data.len(), 2, "one row per direction: {data:?}");

    for group in data {
        assert_eq!(group["name"], "Blue");
        assert!(["IN", "OUT"].contains(&group["direction"].as_str().unwrap()));
        assert_eq!(group["type"], "SYSTEM");
        assert!(group["bitpos"].as_u64().is_some());
        assert_eq!(group["active"], true);
        assert_eq!(
            group["created"].as_str().unwrap().len(),
            10,
            "`created` is a bare date here and epoch millis on the way back in",
        );
    }

    harness.stop().await;
}

#[tokio::test]
async fn switching_a_channel_off_stops_that_device_receiving_on_it() {
    // The property the whole endpoint exists for. A connection holds the rights
    // it authenticated with, so without the re-authentication this asserts the
    // server would carry on routing by the old selection until the device
    // reconnected — while telling the client something else.
    let harness = Harness::start().await;
    let ada = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let bob = harness.enroll("bob", "UID-BOB", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&ada, "PHONE").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;
    drain(&mut bravo).await;
    drain(&mut phone).await;

    bravo.send_sa(51.6, -0.13).await.unwrap();
    phone
        .expect_uid("UID-BOB", EXPECT)
        .await
        .expect("the channel is on, so the position arrives");

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let response = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/groups/active?clientUid=UID-PHONE")
            .insert_header(("authorization", token))
            .insert_header(("content-type", "application/json"))
            .set_payload(
                r#"[{"name":"Blue","direction":"IN","created":1706691425000,"type":"SYSTEM","bitpos":2,"active":true},
                    {"name":"Blue","direction":"OUT","created":1706691425000,"type":"SYSTEM","bitpos":2,"active":false}]"#,
            )
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    bravo.send_sa(51.7, -0.14).await.unwrap();
    phone
        .expect_none(SETTLE)
        .await
        .expect("the channel is off, so nothing should arrive");

    harness.stop().await;
}

#[tokio::test]
async fn the_device_that_made_the_change_is_the_one_not_told_about_it() {
    // `compat/groups.md` §3: the notice makes a client throw away every map
    // item this server gave it, so sending it back to the device that just made
    // the change would undo the change.
    let harness = Harness::start().await;
    let phone_id = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let laptop_id = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&phone_id, "PHONE").await;
    let mut laptop = harness.eud(&laptop_id, "LAPTOP").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    laptop.send_sa(51.6, -0.13).await.unwrap();
    harness.await_callsign("PHONE").await;
    harness.await_callsign("LAPTOP").await;
    drain(&mut phone).await;
    drain(&mut laptop).await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/groups/active?clientUid=UID-PHONE")
            .insert_header(("authorization", token))
            .set_payload(r#"[{"name":"Blue","direction":"OUT","active":false}]"#)
            .to_request(),
    )
    .await;

    let notice = laptop
        .expect(|event| event.r#type == cot_type::GROUP_CHANGE, EXPECT)
        .await
        .expect("the other device is told to re-read its channels");

    assert!(
        notice.uid.ends_with(".UID-PHONE"),
        "the notice names the device that caused it: {}",
        notice.uid,
    );
    phone
        .expect_none(SETTLE)
        .await
        .expect("the device that made the change already knows");

    harness.stop().await;
}

#[tokio::test]
async fn a_change_that_names_no_device_reaches_every_one_of_them() {
    // Design 04 D9, where TAK Server sends nothing at all: CloudTAK never sends
    // a `clientUid`, so a channel toggled from a browser would otherwise never
    // reach the phone looking at the map.
    let harness = Harness::start().await;
    let phone_id = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let laptop_id = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&phone_id, "PHONE").await;
    let mut laptop = harness.eud(&laptop_id, "LAPTOP").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    laptop.send_sa(51.6, -0.13).await.unwrap();
    harness.await_callsign("PHONE").await;
    harness.await_callsign("LAPTOP").await;
    drain(&mut phone).await;
    drain(&mut laptop).await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/groups/active")
            .insert_header(("authorization", token))
            .set_payload(r#"[{"name":"Blue","direction":"OUT","active":false}]"#)
            .to_request(),
    )
    .await;

    for (name, eud) in [("PHONE", &mut phone), ("LAPTOP", &mut laptop)] {
        eud.expect(|event| event.r#type == cot_type::GROUP_CHANGE, EXPECT)
            .await
            .unwrap_or_else(|err| panic!("{name} should be told: {err}"));
    }

    harness.stop().await;
}

#[tokio::test]
async fn asking_for_the_latest_sa_fills_the_map_again() {
    // What ATAK does the instant a `t-x-g-c` arrives, having just discarded
    // every map item this server gave it (`compat/groups.md` §1, §3).
    let harness = Harness::start().await;
    let phone_id = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let laptop_id = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&phone_id, "PHONE").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;

    let mut laptop = harness.eud(&laptop_id, "LAPTOP").await;
    laptop.send_sa(51.6, -0.13).await.unwrap();
    harness.await_callsign("LAPTOP").await;

    // Quiet on both, so that what arrives after the request is the replay
    // rather than the traffic they have already exchanged.
    drain(&mut phone).await;
    drain(&mut laptop).await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/groups/all?useCache=true&sendLatestSA=true")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    phone
        .expect_uid("UID-LAPTOP", EXPECT)
        .await
        .expect("the map is filled in again over the stream, not in this response");

    harness.stop().await;
}

#[tokio::test]
async fn contacts_are_a_bare_array_of_the_people_this_caller_can_reach() {
    // `compat/contacts.md` §1: node-tak calls `.map` on the result with no
    // envelope check, and its formatter trims `notes` unguarded.
    let harness = Harness::start().await;
    let ada = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let bob = harness.enroll("bob", "UID-BOB", &[("Blue", BOTH)]).await;
    let eve = harness.enroll("eve", "UID-EVE", &[("Red", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&ada, "PHONE").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    let mut stranger = harness.eud(&eve, "STRANGER").await;
    for eud in [&mut phone, &mut bravo, &mut stranger] {
        eud.send_sa(51.5, -0.12).await.unwrap();
    }
    for callsign in ["PHONE", "BRAVO", "STRANGER"] {
        harness.await_callsign(callsign).await;
    }

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/contacts/all")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    let contacts = body.as_array().expect("a bare array, never an envelope");
    let uids: Vec<&str> = contacts
        .iter()
        .map(|contact| contact["uid"].as_str().unwrap())
        .collect();

    assert!(uids.contains(&"UID-BOB"), "{uids:?}");
    assert!(
        !uids.contains(&"UID-EVE"),
        "a client in another channel is not a contact: {uids:?}",
    );

    let bravo_row = contacts
        .iter()
        .find(|contact| contact["uid"] == "UID-BOB")
        .unwrap();
    assert_eq!(bravo_row["callsign"], "BRAVO");
    assert_eq!(bravo_row["notes"], "bob");
    assert!(bravo_row["filterGroups"].is_array());
    assert!(bravo_row["team"].is_string());
    assert!(bravo_row["role"].is_string());
    assert!(bravo_row["takv"].is_string());

    harness.stop().await;
}

#[tokio::test]
async fn client_endpoints_says_connected_and_refuses_a_channel_it_cannot_read() {
    // `compat/contacts.md` §2: `lastStatus` is one of two literals or ATAK
    // discards the whole response, and a `group` filter naming a channel the
    // caller cannot read is a hard 403 rather than a silent drop.
    let harness = Harness::start().await;
    let ada = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    harness.enroll("eve", "UID-EVE", &[("Red", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&ada, "PHONE").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/clientEndPoints")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "must-revalidate, max-age=0, no-cache, no-store",
    );

    let body: serde_json::Value = test::read_body_json(response).await;
    assert_eq!(body["type"], "com.bbn.marti.remote.ClientEndpoint");

    let rows = body["data"].as_array().expect("an array of endpoints");
    let mine = rows
        .iter()
        .find(|row| row["uid"] == "UID-PHONE")
        .expect("this caller's own device is on the list");

    assert_eq!(mine["lastStatus"], "Connected");
    assert_eq!(mine["username"], "ada");
    assert!(mine["lastEventTime"].as_str().unwrap().ends_with('Z'));
    assert!(
        rows.iter().all(|row| row["uid"] != "UID-EVE"),
        "a device in another channel is not visible: {rows:?}",
    );

    for (uri, status) in [
        ("/Marti/api/clientEndPoints?group=Blue", 200),
        ("/Marti/api/clientEndPoints?group=Red", 403),
        ("/Marti/api/clientEndPoints?secAgo=-1", 400),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(uri)
                .insert_header(("authorization", token.clone()))
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), status, "{uri}");
    }

    harness.stop().await;
}

#[tokio::test]
async fn a_device_that_has_gone_is_listed_as_disconnected() {
    let harness = Harness::start().await;
    let ada = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let spare = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    // Connected once and gone: which is what puts a row in `devices` with a
    // `last_seen_at`, and is the only thing a disconnected endpoint is made of.
    let laptop = harness.eud(&spare, "LAPTOP").await;
    harness.await_connected(1).await;
    drop(laptop);
    await_only(&harness, 0).await;

    let mut phone = harness.eud(&ada, "PHONE").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;
    drain(&mut phone).await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/clientEndPoints")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    let rows = body["data"].as_array().unwrap();
    let laptop = rows
        .iter()
        .find(|row| row["uid"] == "UID-LAPTOP")
        .expect("a device that enrolled and is not here is still a client endpoint");

    assert_eq!(laptop["lastStatus"], "Disconnected");

    let connected: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/clientEndPoints?showCurrentlyConnectedClients=true")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    let rows = connected["data"].as_array().unwrap();
    assert!(
        rows.iter().all(|row| row["lastStatus"] == "Connected"),
        "{rows:?}",
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_subscription_listing_describes_what_is_connected() {
    let harness = Harness::start().await;
    let ada = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&ada, "PHONE").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri(
                "/Marti/api/subscriptions/all?sortBy=CALLSIGN&direction=ASCENDING&page=-1&limit=-1",
            )
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(body["type"], "SubscriptionInfo");
    let row = &body["data"][0];
    assert_eq!(row["clientUid"], "UID-PHONE");
    assert_eq!(row["callsign"], "PHONE");
    assert_eq!(row["incognito"], false);
    assert!(
        row["metrics"].is_null(),
        "an unknown value is null, not absent"
    );

    let one: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/subscription/UID-PHONE")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;
    assert_eq!(one["data"]["clientUid"], "UID-PHONE");

    let toggled = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/subscriptions/incognito/UID-PHONE")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;
    assert_eq!(toggled.status().as_u16(), 200);
    assert!(
        harness
            .live
            .hub()
            .is_incognito(harness.live.hub().handles_for_uid("UID-PHONE")[0].id()),
        "the toggle is a live property of the connection",
    );

    let missing = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/subscription/UID-NOBODY")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;
    assert_eq!(missing.status().as_u16(), 404);

    harness.stop().await;
}

#[tokio::test]
async fn every_device_of_an_unreachable_account_is_invisible_not_just_the_first() {
    // R-02 H2. The disconnected half of `/clientEndPoints` reads each account's
    // channels once and memoises the answer, because a fleet is a handful of
    // accounts and a great many phones. The memo was written **before** the
    // visibility check and the cache-hit arm had no check at all, so an
    // unreachable account's *first* disconnected device was skipped and every
    // one after it was emitted — leaking that account's username, the device
    // uid, its callsign and its last-seen time to anyone who asked.
    //
    // It takes two devices to see: with one, the miss path runs and the
    // assertion passes. Both of eve's have to connect once, because that is what
    // puts a row in `devices` for the disconnected listing to find.
    let harness = Harness::start().await;
    let ada = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let eve_one = harness.enroll("eve", "UID-EVE", &[("Red", BOTH)]).await;
    let eve_two = harness.enroll("eve", "UID-EVE-2", &[("Red", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    for (identity, callsign) in [(&eve_one, "EVE"), (&eve_two, "EVE-2")] {
        let device = harness.eud(identity, callsign).await;
        harness.await_connected(1).await;
        drop(device);
        await_only(&harness, 0).await;
    }

    let mut phone = harness.eud(&ada, "PHONE").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;
    drain(&mut phone).await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/clientEndPoints")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    let rows = body["data"].as_array().expect("an array of endpoints");

    assert!(
        rows.iter().any(|row| row["uid"] == "UID-PHONE"),
        "the caller's own device is there, so the listing did run: {rows:?}",
    );
    assert!(
        rows.iter()
            .all(|row| row["uid"] != "UID-EVE" && row["uid"] != "UID-EVE-2"),
        "**every** device of an account in another channel is invisible, not just \
         the first one the listing happens to reach: {rows:?}",
    );
    assert!(
        rows.iter().all(|row| row["username"] != "eve"),
        "and the account's name never appears at all: {rows:?}",
    );
}

#[tokio::test]
async fn a_selection_that_changes_nothing_sends_no_notice() {
    // R-02 M3. Design 04 D9 sends `t-x-g-c` even when the request named no
    // `clientUid`, so that a channel toggled from a browser reaches the phone.
    // The unintended consequence: a `t-x-g-c` makes every ATAK on the account
    // **discard its map items and re-fetch** (`compat/groups.md` §3), and
    // CloudTAK's `DataMission.sync` PUTs the whole group list with `active`
    // forced true before it creates a Data Sync (03 §3.4) — so ordinary
    // housekeeping blanked and reloaded every map on the account for a request
    // that applied nothing. D9 stands; the notice is now conditional on the
    // effective selection actually moving.
    let harness = Harness::start().await;
    let phone_id = harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let token = sign_in(&harness, "ada").await;

    let mut phone = harness.eud(&phone_id, "PHONE").await;
    phone.send_sa(51.5, -0.12).await.unwrap();
    harness.await_callsign("PHONE").await;
    drain(&mut phone).await;

    let app = test::init_service(App::new().configure(routes(&harness))).await;
    let active_already = r#"[{"name":"Blue","direction":"OUT","active":true}]"#;

    let response = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/groups/active")
            .insert_header(("authorization", token.clone()))
            .set_payload(active_already)
            .to_request(),
    )
    .await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "the request is applied as usual; only the notice is suppressed",
    );

    // Event-driven rather than timed: a real change afterwards proves the
    // connection was listening all along, and seeing *that* notice first proves
    // the no-op one never came.
    test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/groups/active")
            .insert_header(("authorization", token))
            .set_payload(r#"[{"name":"Blue","direction":"OUT","active":false}]"#)
            .to_request(),
    )
    .await;

    phone
        .expect(|event| event.r#type == cot_type::GROUP_CHANGE, EXPECT)
        .await
        .expect("the change that changed something is announced");

    // And there is no second one: the no-op PUT went first, so a notice for it
    // would already have been read above.
    phone
        .expect_none(SETTLE)
        .await
        .expect("a selection that applied nothing announced nothing");

    harness.stop().await;
}
