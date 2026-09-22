//! The plugin end to end over the live source: FIRMS on the other end of an
//! HTTP connection, and the CoT bytes a device would receive.
//!
//! FIRMS here is a `wiremock` serving a hand-written reply, reached through
//! `[settings.source] base_url`: a test must not hold a MAP_KEY, and an open
//! service is somebody else's rate limit. The plugin is started and ticked the
//! way the harness starts and ticks it, and the assertions are on
//! `rustak_cot::xml::write`, which is what `rustak_client::stream` puts on the
//! wire. The hop from `tick`'s return value to a device is the harness's, and
//! is covered by `rustak-server/tests/feed_sidecars.rs`.

use chrono::Utc;
use rustak_api::ServiceState;
use rustak_client::sidecar::{Sidecar, SidecarConfig, SidecarContext};
use rustak_core::prelude::*;
use rustak_cot::Event;
use rustak_plugin_firms::{FirmsSidecar, Settings};
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Not a real key: the shape of one, so the plugin will put it in a URL.
const KEY: &str = "0123456789abcdef0123456789abcdef";

/// Four detections an hour old: three inside the area below (high, nominal and
/// low confidence) and one 400 km outside it.
fn reply() -> String {
    let seen = Utc::now() - chrono::Duration::hours(1);

    include_str!("fixtures/viirs.csv")
        .replace("{DATE}", &seen.format("%Y-%m-%d").to_string())
        .replace("{HHMM}", &seen.format("%H%M").to_string())
}

/// A mock FIRMS answering every area request the same way.
async fn firms(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/api/area/csv/"))
        .respond_with(response)
        .mount(&server)
        .await;

    server
}

/// The plugin, started over that FIRMS, with the given extra settings.
async fn sidecar(server: &MockServer, settings: &str) -> FirmsSidecar {
    let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "firms"

        [settings.area]
        kind = "circle"
        lat = 40.0
        lon = -8.0
        radius_km = 100.0

        [settings.source]
        kind = "firms"
        map_key = "{KEY}"
        sensors = ["viirs_noaa20"]
        base_url = "{}"

        {settings}
        "#,
        server.uri(),
    ))
    .expect("the sidecar configuration loads");

    let mut plugin = FirmsSidecar::default();
    plugin
        .start(
            SidecarContext::from_config(config, FirmsSidecar::VERSION, Shutdown::new())
                .expect("a usable identity"),
        )
        .await
        .expect("the source opens without reaching its upstream");

    plugin
}

/// What the stream would actually write.
fn wire(event: &Event) -> String {
    String::from_utf8(rustak_cot::xml::write(event).to_vec()).expect("CoT XML is UTF-8")
}

#[tokio::test]
async fn a_firms_reply_becomes_markers_a_device_can_read() {
    let server = firms(ResponseTemplate::new(200).set_body_string(reply())).await;
    let mut plugin = sidecar(&server, "").await;

    let published = plugin.tick().await.expect("the first tick publishes");

    assert_eq!(
        published.len(),
        3,
        "four rows, one of them outside the area"
    );

    let biggest = published
        .iter()
        .find(|event| wire(event).contains("FRP: 112.6 MW"))
        .expect("the 112 MW detection");
    let xml = wire(biggest);

    assert!(biggest.uid.starts_with("FIRMS-N20-"), "{}", biggest.uid);
    assert!(xml.contains(r#"type="b-m-p-s-m""#), "{xml}");
    assert!(xml.contains(r#"how="m-g""#), "{xml}");
    assert!(xml.contains(r#"callsign="Fire "#), "{xml}");
    assert!(xml.contains("Confidence: high"), "{xml}");
    assert!(xml.contains("<color"), "{xml}");
    assert!(
        !biggest.is_stale_at(biggest.time) && biggest.time.millis() < Utc::now().timestamp_millis(),
        "aged from the overpass an hour ago, and still live",
    );

    // What FIRMS was asked: this sensor, the box around the circle, two days,
    // and by a client that says who it is.
    let asked = &server.received_requests().await.expect("recording")[0];
    let path = asked.url.path();

    assert!(
        path.starts_with(&format!("/api/area/csv/{KEY}/VIIRS_NOAA20_NRT/-9.")),
        "{path}",
    );
    assert!(path.ends_with("/2"), "{path}");
    assert!(
        asked.headers["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("rustak-plugin-firms/"),
    );
}

#[tokio::test]
async fn footprints_go_out_as_closed_polygons_beside_their_markers() {
    let server = firms(ResponseTemplate::new(200).set_body_string(reply())).await;
    let mut plugin = sidecar(&server, "[settings.display]\nshape = \"both\"\n").await;

    let published = plugin.tick().await.expect("a tick");
    let footprints: Vec<&Event> = published.iter().filter(|e| e.r#type == "u-d-f").collect();

    assert_eq!(published.len(), 6);
    assert_eq!(footprints.len(), 3);

    let xml = wire(footprints[0]);

    assert_eq!(xml.matches("<link ").count(), 5, "closed: {xml}");
    assert!(
        xml.contains("strokeColor") && xml.contains("fillColor"),
        "{xml}"
    );
}

#[tokio::test]
async fn the_filter_and_the_poll_interval_hold_whatever_the_tick_does() {
    let server = firms(ResponseTemplate::new(200).set_body_string(reply())).await;
    let mut plugin = sidecar(
        &server,
        "[settings.filter]\nmin_confidence = \"nominal\"\nmin_frp_mw = 10.0\n",
    )
    .await;

    assert_eq!(plugin.tick().await.unwrap().len(), 2, "the 3 MW one is out");
    assert!(plugin.tick().await.unwrap().is_empty());
    assert!(plugin.tick().await.unwrap().is_empty());
    assert_eq!(
        server.received_requests().await.expect("recording").len(),
        1,
        "three ticks inside one poll interval is one request",
    );
    assert_eq!(plugin.state(), ServiceState::Healthy);
}

#[tokio::test]
async fn a_refused_key_is_a_setting_to_fix_and_is_never_repeated() {
    // FIRMS refuses in plain text with a 200, and this one echoes the key.
    let server =
        firms(ResponseTemplate::new(200).set_body_string(format!("Invalid MAP_KEY: {KEY}"))).await;
    let mut plugin = sidecar(&server, "").await;

    let published = plugin
        .tick()
        .await
        .expect("an outage never stops a sidecar");
    let beat = plugin.health().await.expect("a started sidecar reports");
    let rendered = format!("{:?} {}", beat.message, beat.metrics);

    assert!(published.is_empty());
    assert_eq!(beat.state, ServiceState::Unhealthy);
    assert!(rendered.contains("Invalid MAP_KEY"), "{rendered}");
    assert!(!rendered.contains(KEY), "{rendered}");
}

#[tokio::test]
async fn being_rate_limited_is_waited_out_rather_than_retried() {
    let server = firms(ResponseTemplate::new(429).insert_header("retry-after", "1800")).await;
    let mut plugin = sidecar(&server, "").await;

    assert!(plugin.tick().await.unwrap().is_empty());
    assert!(plugin.tick().await.unwrap().is_empty());

    let beat = plugin.health().await.expect("a started sidecar reports");

    assert_eq!(
        beat.state,
        ServiceState::Healthy,
        "a 429 is an answer, not a wrong setting",
    );
    assert_eq!(beat.metrics["source"]["rate_limited"], 1);
    assert_eq!(
        server.received_requests().await.expect("recording").len(),
        1
    );
}

#[tokio::test]
async fn a_server_error_is_an_outage_that_names_its_status() {
    let server = firms(ResponseTemplate::new(503).set_body_string("Service Unavailable")).await;
    let mut plugin = sidecar(&server, "").await;

    assert!(plugin.tick().await.unwrap().is_empty());

    let beat = plugin.health().await.expect("a started sidecar reports");

    assert!(beat.message.expect("a message").contains("503"));
}

// `header` is imported for the matcher below: FIRMS is only ever asked by a
// client that identifies itself, so a mock that requires it proves the header
// is on every request rather than on the first.
#[tokio::test]
async fn every_request_identifies_the_plugin() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(header(
            "user-agent",
            rustak_plugin_firms::sources::USER_AGENT,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(reply()))
        .mount(&server)
        .await;

    let mut plugin = sidecar(&server, "").await;

    assert_eq!(plugin.tick().await.unwrap().len(), 3);
}
