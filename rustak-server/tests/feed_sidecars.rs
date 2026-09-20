//! The two feed sidecars, end to end, against a real server.
//!
//! Enrol → connect → register → publish, with a fake EUD on the other side of
//! the channel asserting on the CoT it receives. Every step goes through the
//! thing it is testing: a real certificate authority, a real mutually
//! authenticated `:8089` handshake, `rustak_client::sidecar::drive` driving the
//! plugin crate itself — not a copy of it written out here — and the real
//! `rustak_client::feed` publisher deciding what goes out.
//!
//! The plugins are driven **as libraries**, which is why
//! `rustak-plugin-{ais,adsb}` have a `[lib]` target: a plugin exercised through
//! its own `Sidecar` implementation is the one that ships, whereas a plugin
//! re-implemented in a test file is a test of the test.
//!
//! # `actix_web::test` rather than `tokio::test`
//!
//! `HttpServer::run` needs an actix `System`; see `services_flow.rs`, whose
//! shape this follows.

#![cfg(feature = "testing")]

mod feed_support;
mod stream_support;

use std::time::Duration;

use chrono::Utc;
use rustak_client::feed::{AircraftClass, Track, TrackKind, VesselClass};
use rustak_client::sidecar::Sidecar;
use rustak_plugin_adsb::AdsbSidecar;
use rustak_plugin_ais::AisSidecar;
use rustak_server::prelude::*;

use feed_support::{RunningFeed, replay_settings};
use stream_support::{EXPECT, SETTLE};

/// Five vessels, as an AIS source would have reported them.
fn vessels() -> Vec<Track> {
    let now = Utc::now();

    vec![
        Track::new(
            "AIS-244660000",
            TrackKind::Vessel(VesselClass::Merchant),
            (51.9512, 4.1338),
            now,
        )
        .with_callsign("ZEEBRUGGE")
        .with_velocity(6.2, 271.5)
        .with_remark("MMSI", "244660000"),
        Track::new(
            "AIS-235094384",
            TrackKind::Vessel(VesselClass::Fishing),
            (51.9840, 3.9521),
            now,
        )
        .with_callsign("NORDKAP"),
        Track::new(
            "AIS-244123456",
            TrackKind::Vessel(VesselClass::Leisure),
            (51.8977, 4.0104),
            now,
        )
        .with_callsign("WINDSONG"),
        Track::new(
            "AIS-246789012",
            TrackKind::Vessel(VesselClass::LawEnforcement),
            (51.9203, 4.2711),
            now,
        )
        .with_callsign("RWS PATROL"),
        Track::new(
            "AIS-205123456",
            TrackKind::Vessel(VesselClass::Military),
            (52.0188, 4.0462),
            now,
        )
        .with_callsign("HNLMS HOLLAND"),
    ]
}

/// Five aircraft, as an ADS-B source would have reported them.
fn aircraft() -> Vec<Track> {
    let now = Utc::now();

    vec![
        Track::new(
            "ADSB-3c6444",
            TrackKind::Aircraft(AircraftClass::CivilFixedWing),
            (51.4602, -0.3947),
            now,
        )
        .with_callsign("BAW117")
        .with_altitude_hae_m(762.0)
        .with_velocity(77.2, 88.0),
        Track::new(
            "ADSB-4008f2",
            TrackKind::Aircraft(AircraftClass::CivilFixedWing),
            (51.5510, -0.6218),
            now,
        )
        .with_callsign("EZY84NM"),
        Track::new(
            "ADSB-406b1a",
            TrackKind::Aircraft(AircraftClass::CivilRotary),
            (51.4820, -0.1600),
            now,
        )
        .with_callsign("HLE21"),
        Track::new(
            "ADSB-43c1d2",
            TrackKind::Aircraft(AircraftClass::MilitaryFixedWing),
            (51.6100, -0.7400),
            now,
        )
        .with_callsign("RRR7231"),
        Track::new(
            "ADSB-4ca7b3",
            TrackKind::GroundVehicle,
            (51.4700, -0.4543),
            now,
        )
        .with_callsign("LHR TUG 4")
        .with_on_ground(true),
    ]
}

#[actix_web::test]
async fn the_ais_sidecar_publishes_its_replayed_vessels_to_a_device_on_the_channel() {
    let directory = tempfile::tempdir().expect("a directory for the fixture");
    let (_, settings) = replay_settings(&directory, &vessels());

    let mut feed = RunningFeed::start::<AisSidecar>("ais", &settings).await;

    // The uid, the type, the callsign and the `<track>` are the whole contract
    // between a feed and an operator's map.
    let flagship = feed
        .eud
        .expect_uid("AIS-244660000", EXPECT)
        .await
        .expect("the first vessel arrives");

    assert_eq!(flagship.r#type, "a-u-S-X-M");
    assert_eq!(flagship.callsign(), Some("ZEEBRUGGE"));
    assert_eq!(flagship.how.as_deref(), Some("m-g"));
    assert_eq!(
        flagship.endpoint(),
        None,
        "a ship is a thing on the map, not a chat peer",
    );

    let track: rustak_cot::detail::Track = flagship.detail.get().expect("a <track>");
    assert_eq!(track.speed, 6.2);
    assert_eq!(track.course, 271.5);
    assert!(
        rustak_cot::detail::chat::remarks(&flagship.detail)
            .is_some_and(|remarks| remarks.text.contains("MMSI: 244660000")),
    );
    assert!(!flagship.is_stale_at(flagship.time));

    // And the other four, each as the class it was reported as.
    for (uid, expected, callsign) in [
        ("AIS-235094384", "a-u-S-X-F", "NORDKAP"),
        ("AIS-244123456", "a-u-S-X-R", "WINDSONG"),
        ("AIS-246789012", "a-u-S-X-L", "RWS PATROL"),
        ("AIS-205123456", "a-u-S-C", "HNLMS HOLLAND"),
    ] {
        let event = feed
            .eud
            .expect_uid(uid, EXPECT)
            .await
            .unwrap_or_else(|err| panic!("{uid} should arrive: {err}"));

        assert_eq!(event.r#type, expected, "{uid}");
        assert_eq!(event.callsign(), Some(callsign), "{uid}");
    }

    // It registered itself as well, which is what puts a feed in the admin UI
    // rather than leaving it as an anonymous connection.
    let name = ServiceName::parse("ais").expect("a usable service name");
    let registered = feed
        .harness
        .context
        .db()
        .services()
        .get_by_name(&name)
        .await
        .expect("the services table")
        .expect("the sidecar registered before it started");

    assert_eq!(registered.version.as_deref(), Some(AisSidecar::VERSION));

    feed.stop().await;
}

#[actix_web::test]
async fn the_adsb_sidecar_publishes_its_replayed_aircraft_with_their_altitudes() {
    let directory = tempfile::tempdir().expect("a directory for the fixture");
    let (_, settings) = replay_settings(&directory, &aircraft());

    let mut feed = RunningFeed::start::<AdsbSidecar>("adsb", &settings).await;

    let airliner = feed
        .eud
        .expect_uid("ADSB-3c6444", EXPECT)
        .await
        .expect("the first aircraft arrives");

    assert_eq!(airliner.r#type, "a-u-A-C-F");
    assert_eq!(airliner.callsign(), Some("BAW117"));
    assert_eq!(airliner.point.hae, 762.0);
    assert_eq!(
        airliner.stale.millis() - airliner.time.millis(),
        90_000,
        "the ADS-B plugin's own staleness horizon, not the module default",
    );

    for (uid, expected) in [
        ("ADSB-4008f2", "a-u-A-C-F"),
        ("ADSB-406b1a", "a-u-A-C-H"),
        ("ADSB-43c1d2", "a-u-A-M-F"),
        ("ADSB-4ca7b3", "a-u-G-E-V-C"),
    ] {
        let event = feed
            .eud
            .expect_uid(uid, EXPECT)
            .await
            .unwrap_or_else(|err| panic!("{uid} should arrive: {err}"));

        assert_eq!(event.r#type, expected, "{uid}");
    }

    feed.stop().await;
}

#[actix_web::test]
async fn one_vessel_reported_ten_times_in_a_second_is_published_once() {
    // The policy is the difference between a feed and a flood: a thousand ships
    // reporting every two seconds must not be a thousand messages a second on a
    // channel a phone is holding open over a cellular link.
    let directory = tempfile::tempdir().expect("a directory for the fixture");
    let now = Utc::now();
    let repeated: Vec<Track> = (0..10)
        .map(|nth| {
            Track::new(
                "AIS-244660000",
                TrackKind::Vessel(VesselClass::Merchant),
                // Moving, so that only the interval can be suppressing them.
                (51.9512 + f64::from(nth) * 0.001, 4.1338),
                now + chrono::Duration::milliseconds(i64::from(nth) * 100),
            )
            .with_callsign("ZEEBRUGGE")
        })
        .collect();
    let (_, settings) = replay_settings(&directory, &repeated);

    let mut feed = RunningFeed::start::<AisSidecar>("ais", &settings).await;

    let published = feed
        .eud
        .expect_uid("AIS-244660000", EXPECT)
        .await
        .expect("the vessel arrives once");

    assert_eq!(published.callsign(), Some("ZEEBRUGGE"));

    // `min_interval` is five seconds and the sidecar ticks every second, so
    // nothing more is due for several ticks yet.
    feed.eud
        .expect_none(SETTLE)
        .await
        .expect("nine repeats of one vessel are nine suppressions");

    feed.stop().await;
}

#[actix_web::test]
async fn a_feed_publishes_nothing_for_a_track_outside_its_area() {
    // The area is what the source subscribes with *and* what the publisher
    // re-checks, because an upstream that widens its box is not a reason for a
    // channel to fill up with the Atlantic.
    let directory = tempfile::tempdir().expect("a directory for the fixture");
    let mut tracks = vessels();
    tracks.push(
        Track::new(
            "AIS-999000111",
            TrackKind::Vessel(VesselClass::Merchant),
            (36.1, -5.35),
            Utc::now(),
        )
        .with_callsign("GIBRALTAR"),
    );
    let (_, source) = replay_settings(&directory, &tracks);
    let settings = format!(
        "{source}\n[settings.area]\nkind = \"bbox\"\nsouth = 51.7\nwest = 3.6\nnorth = 52.3\neast = 4.7\n",
    );

    let mut feed = RunningFeed::start::<AisSidecar>("ais", &settings).await;

    feed.eud
        .expect_uid("AIS-244660000", EXPECT)
        .await
        .expect("a vessel inside the box arrives");

    let outsider = feed
        .eud
        .expect(
            |event| event.uid == "AIS-999000111",
            Duration::from_millis(400),
        )
        .await;

    assert!(outsider.is_err(), "the Strait of Gibraltar is not the Maas");

    feed.stop().await;
}
