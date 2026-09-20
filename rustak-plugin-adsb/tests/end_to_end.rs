//! The plugin end to end over a live source: a `readsb` receiver on the other
//! end of an HTTP connection, and the CoT bytes a device would receive.
//!
//! # Why this is not `rustak-server/tests/feed_sidecars.rs`
//!
//! That suite is the one with a real certificate authority, a real `:8089`
//! handshake and a fake EUD on the other side of the channel, and it already
//! proves that this plugin's events reach a device — over the `replay` source
//! M9-00 shipped. It lives in another crate, which this brief does not own and
//! which a second agent is editing in parallel, so the live-source half of the
//! end-to-end story is here instead: the same plugin, started and ticked the
//! way the harness starts and ticks it, with a `readsb` source pointed at a
//! `wiremock` receiver, asserting on `rustak_cot::xml::write` — the exact bytes
//! `rustak_client::stream` puts on the wire.
//!
//! What is therefore *not* covered here is the hop between `tick`'s return
//! value and a device, which is the harness's and is covered there.

use std::time::Duration;

use rustak_client::sidecar::{Sidecar, SidecarConfig, SidecarContext};
use rustak_core::prelude::*;
use rustak_cot::Event;
use rustak_plugin_adsb::{AdsbSidecar, Settings};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The receiver's document, written by hand rather than captured.
const FIXTURE: &str = include_str!("fixtures/readsb.json");

/// A mock `readsb` receiver serving that document at tar1090's path.
async fn receiver() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/data/aircraft.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
        .mount(&server)
        .await;

    server
}

/// The plugin, started over that receiver, with the given extra settings.
async fn sidecar(server: &MockServer, settings: &str) -> AdsbSidecar {
    let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "adsb"

        [settings.source]
        kind = "readsb"
        url_or_path = "{}/data/aircraft.json"
        poll = "1s"

        {settings}
        "#,
        server.uri(),
    ))
    .expect("the sidecar configuration loads");

    let mut plugin = AdsbSidecar::default();
    plugin
        .start(
            SidecarContext::from_config(config, AdsbSidecar::VERSION, Shutdown::new())
                .expect("a usable identity"),
        )
        .await
        .expect("the source opens");

    plugin
}

/// The event with this uid, as XML and as a structure.
fn find<'a>(events: &'a [Event], uid: &str) -> &'a Event {
    events
        .iter()
        .find(|event| event.uid == uid)
        .unwrap_or_else(|| panic!("{uid} should have been published"))
}

/// What the stream would actually write.
fn wire(event: &Event) -> String {
    String::from_utf8(rustak_cot::xml::write(event).to_vec()).expect("CoT XML is UTF-8")
}

#[tokio::test]
async fn a_readsb_receiver_becomes_cot_a_device_can_read() {
    let server = receiver().await;
    let mut plugin = sidecar(&server, "").await;

    let published = plugin.tick().await.expect("the first tick publishes");

    assert_eq!(
        published.len(),
        5,
        "seven aircraft, of which one has no position and one is stale",
    );

    let airliner = find(&published, "ADSB-3c6444");
    let xml = wire(airliner);

    assert_eq!(airliner.r#type, "a-u-A-C-F");
    assert_eq!(airliner.callsign(), Some("BAW117"));

    // 3225 ft geometric, which is what `hae` means: height above the ellipsoid.
    assert!(
        (airliner.point.hae - 982.98).abs() < 0.01,
        "{}",
        airliner.point.hae
    );
    assert_eq!(
        airliner.stale.millis() - airliner.time.millis(),
        90_000,
        "the ADS-B plugin's own staleness horizon",
    );

    assert!(xml.contains(r#"uid="ADSB-3c6444""#), "{xml}");
    assert!(xml.contains(r#"type="a-u-A-C-F""#), "{xml}");
    assert!(xml.contains(r#"how="m-g""#), "{xml}");
    assert!(xml.contains(r#"callsign="BAW117""#), "{xml}");
    assert!(xml.contains("<track"), "{xml}");
    assert!(xml.contains(r#"course="88"#), "{xml}");
    assert!(xml.contains(r#"speed="127.3"#), "247.6 kt in m/s: {xml}");
    assert!(
        xml.contains("Registration: G-XLEA"),
        "the remarks are what an operator taps a track to read: {xml}",
    );
    assert!(
        !xml.contains("endpoint="),
        "an aircraft is a thing on the map, not a chat peer: {xml}",
    );
}

#[tokio::test]
async fn each_kind_of_target_gets_the_type_its_symbol_comes_from() {
    let server = receiver().await;
    let mut plugin = sidecar(&server, "").await;

    let published = plugin.tick().await.expect("a tick");

    for (uid, expected, callsign) in [
        ("ADSB-3c6444", "a-u-A-C-F", "BAW117"),
        ("ADSB-406b1a", "a-u-A-C-H", "HLE21"),
        ("ADSB-43c1d2", "a-u-A-M-F", "RRR7231"),
        ("ADSB-4ca7b3", "a-u-G-E-V-C", "LHRTUG4"),
        ("ADSB-~a1b2c3", "a-u-A", "~a1b2c3"),
    ] {
        let event = find(&published, uid);

        assert_eq!(event.r#type, expected, "{uid}");
        assert_eq!(event.callsign(), Some(callsign), "{uid}");
    }
}

#[tokio::test]
async fn an_aircraft_on_the_ground_carries_no_altitude() {
    let server = receiver().await;
    let mut plugin = sidecar(&server, "").await;

    let published = plugin.tick().await.expect("a tick");
    let tug = find(&published, "ADSB-4ca7b3");

    assert!(
        (tug.point.hae - 9_999_999.0).abs() < 0.5,
        "the `ground` sentinel is not a number of feet: {}",
        tug.point.hae,
    );
    assert!(
        wire(tug).contains("Altitude: on the ground"),
        "{}",
        wire(tug)
    );
}

#[tokio::test]
async fn the_poll_interval_is_a_floor_under_the_requests_we_make() {
    // The sidecar's tick and the upstream's rate are separate numbers, and this
    // is what keeps them that way: ten ticks inside one poll interval is one
    // request, not ten.
    let server = receiver().await;
    let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "adsb"

        [settings.source]
        kind = "readsb"
        url_or_path = "{}/data/aircraft.json"
        poll = "1h"
        "#,
        server.uri(),
    ))
    .expect("the configuration loads");

    let mut plugin = AdsbSidecar::default();
    plugin
        .start(
            SidecarContext::from_config(config, AdsbSidecar::VERSION, Shutdown::new())
                .expect("a usable identity"),
        )
        .await
        .expect("the source opens");

    for _ in 0..10 {
        let _ = plugin.tick().await.expect("a tick");
    }

    assert_eq!(
        server
            .received_requests()
            .await
            .expect("a request log")
            .len(),
        1,
        "ten ticks, one request",
    );
}

#[tokio::test]
async fn an_aircraft_outside_the_configured_area_is_never_published() {
    let server = receiver().await;
    let mut plugin = sidecar(
        &server,
        "[settings.area]\nkind = \"circle\"\nlat = 51.4775\nlon = -0.4614\nradius_km = 15.0\n",
    )
    .await;

    let published = plugin.tick().await.expect("a tick");
    let uids: Vec<&str> = published.iter().map(|event| event.uid.as_str()).collect();

    assert!(uids.contains(&"ADSB-3c6444"), "{uids:?}");
    assert!(uids.contains(&"ADSB-4ca7b3"), "{uids:?}");
    assert!(
        !uids.contains(&"ADSB-~a1b2c3"),
        "that one is 60 km away: {uids:?}",
    );
}

#[tokio::test]
async fn every_request_to_a_receiver_says_who_we_are() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header(
            "user-agent",
            rustak_plugin_adsb::sources::USER_AGENT,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
        .mount(&server)
        .await;

    let mut plugin = sidecar(&server, "").await;

    assert_eq!(
        plugin.tick().await.expect("a matching request").len(),
        5,
        "an unmatched user agent would have been a 404 and no tracks",
    );
}

#[tokio::test]
async fn a_receiver_that_is_down_is_a_log_line_rather_than_a_stopped_sidecar() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&server)
        .await;

    let mut plugin = sidecar(&server, "").await;

    assert!(
        plugin
            .tick()
            .await
            .expect("the tick still succeeds")
            .is_empty(),
        "an upstream that is down must never stop a sidecar",
    );
    assert_eq!(
        plugin.state(),
        rustak_api::ServiceState::Unhealthy,
        "and the heartbeat is what says so",
    );

    plugin.stop().await.expect("it stops cleanly");
}

#[tokio::test]
async fn a_second_tick_inside_the_interval_publishes_nothing_new() {
    let server = receiver().await;
    let mut plugin = sidecar(&server, "").await;

    assert_eq!(plugin.tick().await.expect("the first tick").len(), 5);

    // Nothing has moved 50 m and `min_interval` has not passed, so the policy
    // turns a receiver that reports once a second back into a rate.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        plugin.tick().await.expect("the second tick").is_empty(),
        "the publisher is what makes a feed a rate rather than a flood",
    );
    assert_eq!(plugin.counters().published, 5);
    assert_eq!(plugin.tracked(), 5);
}

#[tokio::test]
async fn the_sidecar_reports_its_feed_and_its_upstream_through_the_health_hook() {
    // What an administrator sees on the Services page. `Sidecar::health` is
    // where it comes from since M9-04: the harness asks after every tick and
    // sends the answer *instead of* its own `Heartbeat::healthy()`, so this
    // plugin no longer posts a heartbeat of its own and nothing overwrites
    // what it says.
    let receiver = receiver().await;
    let control = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/services/adsb/heartbeat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"state":"healthy","last_heartbeat_at":"2026-09-20T12:00:00.000Z"}"#,
        ))
        .mount(&control)
        .await;
    // `config` is read once at start-up for an `area` an administrator set;
    // answering `{}` is a server that has none.
    Mock::given(method("GET"))
        .and(path("/api/v1/services/adsb/config"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&control)
        .await;

    let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "adsb"
        token = "not-a-real-token"

        [server]
        control = "{}"

        [settings.source]
        kind = "readsb"
        url_or_path = "{}/data/aircraft.json"
        poll = "1s"
        "#,
        control.uri(),
        receiver.uri(),
    ))
    .expect("the configuration loads");

    let mut plugin = AdsbSidecar::default();
    plugin
        .start(
            SidecarContext::from_config(config, AdsbSidecar::VERSION, Shutdown::new())
                .expect("a usable identity"),
        )
        .await
        .expect("the source opens");

    assert_eq!(plugin.tick().await.expect("a tick").len(), 5);

    let beat = plugin.health().await.expect("a started sidecar reports");

    assert_eq!(beat.state, rustak_api::ServiceState::Healthy);
    assert_eq!(beat.metrics["source"]["kind"], "readsb");
    assert_eq!(beat.metrics["source"]["connection"], "connected");
    assert_eq!(beat.metrics["tracked"], 5);
    assert_eq!(beat.metrics["feed"]["published"], 5);
    assert!(
        beat.message
            .as_deref()
            .expect("a message")
            .contains("5 aircraft"),
        "{beat:?}",
    );

    // And nothing was posted from inside the plugin: the harness makes the one
    // request, from what the hook answered, which is what stopped this report
    // being overwritten a millisecond after it landed.
    let posted = control
        .received_requests()
        .await
        .expect("a request log")
        .iter()
        .filter(|request| request.url.path().ends_with("/heartbeat"))
        .count();

    assert_eq!(posted, 0, "the hook is a value, not a request");
}

#[tokio::test]
async fn an_area_an_administrator_set_wins_over_the_one_in_the_file() {
    let receiver = receiver().await;
    let control = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/services/adsb/config"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"area":{"kind":"circle","lat":51.4775,"lon":-0.4614,"radius_km":15.0}}"#,
        ))
        .mount(&control)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"state":"healthy"}"#))
        .mount(&control)
        .await;

    let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "adsb"
        token = "not-a-real-token"

        [server]
        control = "{}"

        [settings.area]
        kind = "bbox"
        south = -90.0
        west = -180.0
        north = 90.0
        east = 180.0

        [settings.source]
        kind = "readsb"
        url_or_path = "{}/data/aircraft.json"
        poll = "1s"
        "#,
        control.uri(),
        receiver.uri(),
    ))
    .expect("the configuration loads");

    let mut plugin = AdsbSidecar::default();
    plugin
        .start(
            SidecarContext::from_config(config, AdsbSidecar::VERSION, Shutdown::new())
                .expect("a usable identity"),
        )
        .await
        .expect("the source opens");

    let uids: Vec<String> = plugin
        .tick()
        .await
        .expect("a tick")
        .iter()
        .map(|event| event.uid.clone())
        .collect();

    assert!(
        !uids.iter().any(|uid| uid == "ADSB-~a1b2c3"),
        "the file says the whole world; the server says 15 km: {uids:?}",
    );
    assert!(uids.iter().any(|uid| uid == "ADSB-3c6444"), "{uids:?}");
}
