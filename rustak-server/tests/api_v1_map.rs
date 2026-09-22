//! `/api/v1/map`: the snapshot a map draws from, and the feed that keeps it
//! current.
//!
//! All three are exercised in-process. The snapshot is read back from rows
//! written the way the store's writer writes them, and a track from segments
//! written the way the history writer writes them; the feed is driven through
//! the router's own tap, which is the only thing it listens to, so nothing
//! here needs a TLS listener or a connected device.

#![cfg(feature = "testing")]

use std::sync::Arc;

use std::pin::Pin;
use std::time::Duration as StdDuration;

use actix_web::body::MessageBody;
use actix_web::http::StatusCode;
use actix_web::{App, test};
use chrono::{DateTime, Duration, Utc};
use rustak_api::{MapFeature, MapPoint, PublishFeature};
use rustak_cot::codec::EncodedEvent;
use rustak_cot::detail::{Chat, Contact, Element, Group};
use rustak_cot::{CotTime, Event};
use rustak_server::cot_store::history::HistoryWriter;
use rustak_server::cot_store::latest::upsert_batch;
use rustak_server::cot_store::{self, CotRecord, CotStoreOptions};
use rustak_server::prelude::*;
use rustak_server::stream::mission_hook::no_missions;
use rustak_server::stream::{Hub, LiveState, Router, StreamMetrics};
use rustak_server::testing::TestServer;
use rustak_server::testing::context::bearer;

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// Somebody publishing into `bits`.
fn sender(bits: &[u32]) -> Principal {
    let mut groups = GroupSet::new();
    for bitpos in bits {
        groups.set(*bitpos, Direction::In);
    }

    Principal::new(
        UserId::from(1),
        Username::parse("grace").unwrap(),
        PrincipalKind::Person,
        AuthMethod::SetupToken,
    )
    .with_groups(Arc::new(groups))
}

/// A message of `kind`, sent now and stale at `stale`.
fn message(uid: &str, kind: &str, stale: DateTime<Utc>) -> Arc<EncodedEvent> {
    Arc::new(EncodedEvent::new(
        Event::builder(kind, uid)
            .how("m-g")
            .point(51.5, -0.12)
            .stale(CotTime::from_datetime(stale))
            .typed(&Contact::new("ALPHA"))
            .typed(&Group::new("Cyan", "Team Member"))
            .push(
                Element::new("track")
                    .attr("course", "90")
                    .attr("speed", "2"),
            )
            .build(),
    ))
}

async fn store(server: &TestServer, encoded: Arc<EncodedEvent>, bits: &[u32]) {
    let mut record = CotRecord::new(encoded, &sender(bits), None);
    // No account row exists for the foreign key to point at, and none of
    // these reads is about the sender's row.
    record.user_id = None;

    upsert_batch(server.db(), vec![record])
        .await
        .expect("a stored message");
}

/// One fix of a track: where `uid` was at `time`, heading `course`.
fn fix(uid: &str, time: DateTime<Utc>, lon: f64, course: u32) -> Arc<EncodedEvent> {
    Arc::new(EncodedEvent::new(
        Event::builder("a-f-G-U-C", uid)
            .how("m-g")
            .point(51.5, lon)
            .time(CotTime::from_datetime(time))
            .start(CotTime::from_datetime(time))
            .stale(CotTime::from_datetime(time + Duration::minutes(2)))
            .typed(&Contact::new("ALPHA"))
            .push(
                Element::new("track")
                    .attr("course", course.to_string())
                    .attr("speed", "2"),
            )
            .build(),
    ))
}

/// Writes `fixes` into `uid`'s history segments, the way the store's writer
/// does, and leaves the index describing them.
async fn record(server: &TestServer, uid: &str, fixes: &[Arc<EncodedEvent>]) {
    let mut writer = HistoryWriter::new(server.db().clone(), server.config().streams_dir());

    for encoded in fixes {
        let time = encoded.event().time.to_datetime().unwrap();
        writer
            .append(uid, time, encoded.proto())
            .await
            .expect("a stored fix");
    }

    writer.close().await.expect("the index catches up");
}

async fn features(server: &TestServer, token: &rustak_api::TokenResponse) -> Vec<MapFeature> {
    let app = app!(server);

    test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/map/features")
            .insert_header(("authorization", bearer(token)))
            .to_request(),
    )
    .await
}

/// Gives the server a stream to watch, recording what it relays the way the
/// real one does, and answers the router feeding it.
async fn streaming(server: &TestServer) -> Arc<Router> {
    let hub = Arc::new(Hub::new());
    let metrics = Arc::new(StreamMetrics::default());
    let (store, _writer) = cot_store::start(
        server.db().clone(),
        server.config().streams_dir(),
        CotStoreOptions {
            window: StdDuration::from_millis(10),
            ..CotStoreOptions::default()
        },
        server.context.shutdown().clone(),
    );
    let router = Arc::new(Router::new(
        Arc::clone(&hub),
        server.db().clone(),
        store.clone(),
        no_missions(),
        Arc::clone(&metrics),
        "rustak-test",
    ));

    server
        .context
        .install_live(Arc::new(LiveState::new(
            hub,
            Arc::clone(&router),
            store,
            metrics,
        )))
        .expect("the stream is installed once");

    router
}

/// Waits for the store's writer to have written `uid`'s latest row.
async fn await_stored(server: &TestServer, uid: &str) {
    for _ in 0..200 {
        if rustak_server::cot_store::latest_event(server.db(), uid)
            .await
            .expect("a readable store")
            .is_some()
        {
            return;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }

    panic!("{uid} was never recorded");
}

/// Everything the feed wrote between being opened and the server stopping.
async fn watched(
    server: &TestServer,
    token: &rustak_api::TokenResponse,
    relay: impl FnOnce(),
) -> String {
    let app = app!(server);
    let response = test::TestRequest::get()
        .uri("/api/v1/map/events")
        .insert_header(("authorization", bearer(token)))
        .send_request(&app)
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    relay();

    // The shutdown ends the response, so the body is what was relayed and stops.
    server.context.shutdown().cancel();
    String::from_utf8(test::read_body(response).await.to_vec()).unwrap()
}

#[actix_web::test]
async fn the_snapshot_is_what_is_current_and_worth_drawing() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let soon = Utc::now() + Duration::minutes(2);

    store(&server, message("ANDROID-1", "a-f-G-U-C", soon), &[2]).await;
    store(
        &server,
        message("LONG-GONE", "a-f-G-U-C", Utc::now() - Duration::hours(1)),
        &[2],
    )
    .await;
    store(
        &server,
        Arc::new(EncodedEvent::new(
            Event::builder("b-t-f", "GeoChat.ANDROID-1.All.1")
                .stale(CotTime::from_datetime(soon))
                .typed(&Chat::default())
                .build(),
        )),
        &[2],
    )
    .await;

    let listed = features(&server, &admin).await;

    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].uid, "ANDROID-1");
    assert_eq!(listed[0].callsign.as_deref(), Some("ALPHA"));
    assert_eq!(listed[0].team.as_deref(), Some("Cyan"));
    assert_eq!(listed[0].course, Some(90.0));
    assert_eq!((listed[0].point.lat, listed[0].point.lon), (51.5, -0.12));
}

#[actix_web::test]
async fn a_track_is_the_history_read_back_oldest_first_and_narrowed_to_a_window() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let now = Utc::now();

    store(
        &server,
        message("ANDROID-1", "a-f-G-U-C", now + Duration::minutes(2)),
        &[2],
    )
    .await;
    record(
        &server,
        "ANDROID-1",
        &[
            fix("ANDROID-1", now - Duration::seconds(120), -0.12, 90),
            fix("ANDROID-1", now - Duration::seconds(60), -0.11, 95),
            fix("ANDROID-1", now, -0.10, 100),
        ],
    )
    .await;

    let app = app!(server);
    let track = |query: &'static str| {
        test::TestRequest::get()
            .uri(&format!("/api/v1/map/features/ANDROID-1/history{query}"))
            .insert_header(("authorization", bearer(&admin)))
            .to_request()
    };

    let listed: Vec<MapFeature> = test::call_and_read_body_json(&app, track("?secago=3600")).await;

    let lons: Vec<f64> = listed.iter().map(|fix| fix.point.lon).collect();
    assert_eq!(lons, [-0.12, -0.11, -0.10], "{listed:?}");
    assert!(listed.windows(2).all(|pair| pair[0].time < pair[1].time));
    assert_eq!(listed[0].course, Some(90.0));
    assert_eq!(listed[0].callsign.as_deref(), Some("ALPHA"));

    let recent: Vec<MapFeature> = test::call_and_read_body_json(&app, track("?secago=90")).await;
    assert_eq!(recent.len(), 2, "{recent:?}");

    let newest: Vec<MapFeature> =
        test::call_and_read_body_json(&app, track("?secago=3600&limit=1")).await;
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].point.lon, -0.10, "the newest fix is the one kept");
}

#[actix_web::test]
async fn a_track_is_gated_the_way_the_snapshot_is() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let now = Utc::now();

    // Bit 9 is a channel nobody in this test holds in the `OUT` direction.
    store(
        &server,
        message("UID-SECRET", "a-f-G-U-C", now + Duration::minutes(2)),
        &[9],
    )
    .await;
    record(&server, "UID-SECRET", &[fix("UID-SECRET", now, -0.12, 90)]).await;

    let app = app!(server);
    let track = |uid: &str, token: &rustak_api::TokenResponse| {
        test::TestRequest::get()
            .uri(&format!("/api/v1/map/features/{uid}/history?secago=3600"))
            .insert_header(("authorization", bearer(token)))
            .to_request()
    };

    let refused = test::call_service(&app, track("UID-SECRET", &ordinary)).await;
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    let unknown = test::call_service(&app, track("NOBODY", &admin)).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let listed: Vec<MapFeature> =
        test::call_and_read_body_json(&app, track("UID-SECRET", &admin)).await;
    assert_eq!(listed.len(), 1);
}

fn marker(callsign: &str, groups: &[&str]) -> PublishFeature {
    PublishFeature {
        kind: "a-u-G".to_string(),
        callsign: callsign.to_string(),
        point: MapPoint {
            lat: 51.51,
            lon: -0.11,
            hae: Some(20.0),
            ce: None,
            le: None,
        },
        how: None,
        remarks: Some("Placed from the console.".to_string()),
        sidc: None,
        stale: None,
        groups: groups.iter().map(|group| (*group).to_string()).collect(),
    }
}

#[actix_web::test]
async fn a_marker_published_from_the_console_is_relayed_recorded_and_can_be_forgotten() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let router = streaming(&server).await;
    let app = app!(server);
    let mut seen = router.tap().subscribe();

    let published: MapFeature = test::call_and_read_body_json(
        &app,
        test::TestRequest::put()
            .uri("/api/v1/map/features/MARKER-1")
            .insert_header(("authorization", bearer(&admin)))
            .set_json(marker("CONTACT 1", &[]))
            .to_request(),
    )
    .await;

    assert_eq!(published.uid, "MARKER-1");
    assert_eq!(published.callsign.as_deref(), Some("CONTACT 1"));
    assert_eq!(published.how.as_deref(), Some("h-g-i-g-o"));
    assert_eq!(published.point.hae, Some(20.0));
    assert_eq!(
        published.remarks.as_deref(),
        Some("Placed from the console.")
    );
    assert!(published.stale > Utc::now() + Duration::hours(23));

    // What every open map was told is what was answered.
    let relayed = seen.try_recv().expect("the tap saw the marker");
    assert_eq!(relayed.encoded.event().uid, "MARKER-1");
    assert!(seen.try_recv().is_err(), "one message, relayed once");

    // Replacing it is the same uid again.
    let renamed: MapFeature = test::call_and_read_body_json(
        &app,
        test::TestRequest::put()
            .uri("/api/v1/map/features/MARKER-1")
            .insert_header(("authorization", bearer(&admin)))
            .set_json(marker("CONTACT ONE", &[]))
            .to_request(),
    )
    .await;
    assert_eq!(renamed.callsign.as_deref(), Some("CONTACT ONE"));

    // The stand-in connection did not stay.
    assert!(router.hub().is_empty());

    await_stored(&server, "MARKER-1").await;
    let deleted = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri("/api/v1/map/features/MARKER-1")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    // A delete is the `t-x-d-d` a device understands, naming the marker.
    let _replacement = seen.try_recv().expect("the replacement was relayed");
    let forget = seen.try_recv().expect("the delete was relayed");
    assert_eq!(forget.encoded.event().r#type, "t-x-d-d");
    let xml = String::from_utf8_lossy(forget.encoded.xml());
    assert!(xml.contains("uid=\"MARKER-1\""), "{xml}");
    assert!(xml.contains("<__forcedelete/>"), "{xml}");

    let gone = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri("/api/v1/map/features/MARKER-1")
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;
    assert_eq!(
        gone.status(),
        StatusCode::NOT_FOUND,
        "nothing is held any more"
    );
}

#[actix_web::test]
async fn a_marker_is_refused_where_a_device_would_be() {
    let server = TestServer::start().await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    streaming(&server).await;
    let app = app!(server);

    let publish = |uid: &str, draft: PublishFeature| {
        test::TestRequest::put()
            .uri(&format!("/api/v1/map/features/{uid}"))
            .insert_header(("authorization", bearer(&ordinary)))
            .set_json(draft)
            .to_request()
    };

    let no_such_channel = test::call_service(&app, publish("M-1", marker("M", &["Nowhere"]))).await;
    assert_eq!(no_such_channel.status(), StatusCode::NOT_FOUND);

    let chat = test::call_service(
        &app,
        publish(
            "M-2",
            PublishFeature {
                kind: "b-t-f".to_string(),
                ..marker("M", &[])
            },
        ),
    )
    .await;
    assert_eq!(chat.status(), StatusCode::BAD_REQUEST);

    let nonsense = test::call_service(
        &app,
        publish(
            "M-3",
            PublishFeature {
                kind: "a-u-G;drop".to_string(),
                ..marker("M", &[])
            },
        ),
    )
    .await;
    assert_eq!(nonsense.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn a_server_with_no_stream_cannot_publish() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let refused = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/api/v1/map/features/MARKER-1")
            .insert_header(("authorization", bearer(&admin)))
            .set_json(marker("M", &[]))
            .to_request(),
    )
    .await;

    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[actix_web::test]
async fn the_snapshot_is_narrowed_to_the_channels_the_reader_receives() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;

    // Bit 9 is a channel nobody in this test holds in the `OUT` direction.
    let soon = Utc::now() + Duration::minutes(2);
    store(&server, message("UID-SECRET", "a-f-G-U-C", soon), &[9]).await;

    assert!(features(&server, &ordinary).await.is_empty());
    assert_eq!(features(&server, &admin).await.len(), 1);
}

#[actix_web::test]
async fn a_server_with_no_stream_says_there_is_nothing_live_to_watch() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let response = test::TestRequest::get()
        .uri("/api/v1/map/events")
        .insert_header(("authorization", bearer(&admin)))
        .send_request(&app)
        .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[actix_web::test]
async fn the_feed_carries_what_is_relayed_and_what_is_deleted() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let router = streaming(&server).await;
    let soon = Utc::now() + Duration::minutes(2);

    let body = watched(&server, &admin, || {
        let tap = router.tap();

        tap.publish(&message("ANDROID-1", "a-f-G-U-C", soon), &sender(&[2]));
        tap.publish(&message("PING", "t-x-c-t", soon), &sender(&[2]));
        tap.publish(
            &Arc::new(EncodedEvent::new(
                Event::builder("t-x-d-d", "throwaway")
                    .push(Element::new("link").attr("uid", "MARKER-1"))
                    .build(),
            )),
            &sender(&[2]),
        );
    })
    .await;

    assert!(body.starts_with("retry: 5000\n\n"), "{body}");
    assert!(body.contains("event: upsert\ndata: {"), "{body}");
    assert!(body.contains("\"uid\":\"ANDROID-1\""), "{body}");
    assert!(body.contains("event: remove\ndata: {"), "{body}");
    assert!(body.contains("\"uid\":\"MARKER-1\""), "{body}");
    assert!(!body.contains("PING"), "{body}");
}

#[actix_web::test]
async fn the_feed_is_narrowed_the_way_the_snapshot_is() {
    let server = TestServer::start().await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let router = streaming(&server).await;
    let soon = Utc::now() + Duration::minutes(2);

    let body = watched(&server, &ordinary, || {
        router
            .tap()
            .publish(&message("UID-SECRET", "a-f-G-U-C", soon), &sender(&[9]));
    })
    .await;

    assert_eq!(body, "retry: 5000\n\n", "{body}");
}

/// The next thing the feed writes, for a test that has to look at an open feed
/// rather than at everything a closed one said.
async fn chunk<B: MessageBody>(body: &mut Pin<Box<B>>) -> Option<String> {
    std::future::poll_fn(|cx| body.as_mut().poll_next(cx))
        .await
        .and_then(Result::ok)
        .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
}

#[actix_web::test]
async fn an_open_map_ends_when_its_account_is_switched_off() {
    // The response outlives the request that opened it, so a revocation has to
    // reach a map somebody left open. Nothing cancels the shutdown token here:
    // the response ends because the credential was checked again and refused.
    let server = TestServer::start().await;
    let (user, admin) = server.signed_in("grace", true).await;
    streaming(&server).await;
    let app = app!(server);

    let response = test::TestRequest::get()
        .uri("/api/v1/map/events")
        .insert_header(("authorization", bearer(&admin)))
        .send_request(&app)
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    server
        .db()
        .users()
        .set_disabled(user.id, true)
        .await
        .unwrap();
    server.context.events().invalidate(&user.username);

    let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert_eq!(body, "retry: 5000\n\n", "{body}");
}

#[actix_web::test]
async fn a_map_whose_account_is_still_good_carries_on_after_being_checked() {
    let server = TestServer::start().await;
    let (user, admin) = server.signed_in("grace", true).await;
    let router = streaming(&server).await;
    let app = app!(server);
    let soon = Utc::now() + Duration::minutes(2);

    let response = test::TestRequest::get()
        .uri("/api/v1/map/events")
        .insert_header(("authorization", bearer(&admin)))
        .send_request(&app)
        .await;
    let mut body = Box::pin(response.into_body());

    assert_eq!(chunk(&mut body).await.as_deref(), Some("retry: 5000\n\n"));

    // Asked to check, with nothing wrong. The feed has nothing to write while
    // it does, so it is driven until it goes quiet rather than until it speaks.
    server.context.events().invalidate(&user.username);
    let quiet = tokio::time::timeout(StdDuration::from_millis(500), chunk(&mut body)).await;
    assert!(
        quiet.is_err(),
        "a check that passes writes nothing: {quiet:?}"
    );

    router
        .tap()
        .publish(&message("ANDROID-1", "a-f-G-U-C", soon), &sender(&[2]));

    let next = chunk(&mut body).await.expect("the feed is still open");
    assert!(next.starts_with("event: upsert\n"), "{next}");
    assert!(next.contains("\"uid\":\"ANDROID-1\""), "{next}");
}

#[actix_web::test]
async fn a_map_that_falls_behind_is_told_to_start_again() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let router = streaming(&server).await;
    let soon = Utc::now() + Duration::minutes(2);

    // More than the tap's ring holds, before the page reads any of it.
    let body = watched(&server, &admin, || {
        let relayed = message("ANDROID-1", "a-f-G-U-C", soon);
        for _ in 0..1_100 {
            router.tap().publish(&relayed, &sender(&[2]));
        }
    })
    .await;

    assert!(
        body.contains("event: reset\ndata: {\"op\":\"reset\"}\n\n"),
        "{body}"
    );
}

#[actix_web::test]
async fn only_so_many_maps_may_be_open_at_once() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    streaming(&server).await;
    let app = app!(server);

    let open = || {
        test::TestRequest::get()
            .uri("/api/v1/map/events")
            .insert_header(("authorization", bearer(&admin)))
            .send_request(&app)
    };

    // Held, because a feed gives its place back when its response is dropped.
    let mut held = Vec::new();
    for _ in 0..64 {
        let response = open().await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }

    assert_eq!(open().await.status(), StatusCode::SERVICE_UNAVAILABLE);

    drop(held);
    assert_eq!(open().await.status(), StatusCode::OK);
}
