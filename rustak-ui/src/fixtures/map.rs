//! A map's worth of sample situation, around the same part of London the rest
//! of the fixtures live in: people on teams, tracks of every affiliation, a
//! marker, a drawing, a route, and something that has stopped reporting.
//!
//! The feed is here too. Demo mode has no stream behind it, so [`map_tick`]
//! stands in for one — an aircraft flies a circuit and a person walks — which
//! is what makes the page reviewable as the *live* page it is. [`map_history`]
//! is the same two, walked backwards, so that a track has something to draw.

use std::cell::RefCell;

use chrono::{DateTime, Duration, Utc};
use rustak_api::{MapFeature, MapPoint, MapShape, MapUpdate, PublishFeature};

use crate::api::ApiError;

thread_local! {
    /// What the demo has placed on the map, which lives as long as the tab does.
    static PUBLISHED: RefCell<Vec<MapFeature>> = const { RefCell::new(Vec::new()) };

    /// What the demo has deleted of what it started with.
    static REMOVED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn ago(seconds: i64) -> DateTime<Utc> {
    Utc::now() - Duration::seconds(seconds)
}

fn feature(uid: &str, kind: &str, callsign: &str, lat: f64, lon: f64) -> MapFeature {
    MapFeature {
        uid: uid.to_string(),
        kind: kind.to_string(),
        how: Some("m-g".to_string()),
        callsign: Some(callsign.to_string()),
        team: None,
        role: None,
        time: ago(20),
        stale: ago(-600),
        received_at: ago(20),
        point: MapPoint {
            lat,
            lon,
            hae: Some(24.0),
            ce: Some(8.0),
            le: None,
        },
        shape: None,
        course: None,
        speed: None,
        battery: None,
        remarks: None,
        software: None,
        sidc: None,
        groups: vec!["Blue Team".to_string()],
    }
}

fn person(uid: &str, callsign: &str, team: &str, role: &str, lat: f64, lon: f64) -> MapFeature {
    MapFeature {
        team: Some(team.to_string()),
        role: Some(role.to_string()),
        battery: Some(74),
        software: Some("ATAK-CIV 5.2.0 · Pixel 8".to_string()),
        groups: vec!["Command".to_string(), "Blue Team".to_string()],
        ..feature(uid, "a-f-G-U-C", callsign, lat, lon)
    }
}

/// The aircraft, `tick` steps around its circuit.
fn rescue(tick: u32) -> MapFeature {
    rescue_at(f64::from(tick), Utc::now())
}

/// The aircraft where it is `tick` steps in — negative for steps before the
/// page opened — reported at `time`.
fn rescue_at(tick: f64, time: DateTime<Utc>) -> MapFeature {
    let around = tick * 0.12;

    MapFeature {
        time,
        received_at: time,
        course: Some((around.to_degrees() + 90.0).rem_euclid(360.0)),
        speed: Some(62.0),
        point: MapPoint {
            lat: 51.512 + 0.035 * around.cos(),
            lon: -0.105 + 0.06 * around.sin(),
            hae: Some(450.0),
            ce: Some(15.0),
            le: Some(20.0),
        },
        remarks: Some("HEMS, inbound to the Royal London.".to_string()),
        // Published by a device set to 2525D, so it says which symbol it means.
        sidc: Some("10030100001202000000".to_string()),
        groups: vec!["Air".to_string()],
        ..feature("ICAO-406b2f", "a-f-A-C-H", "RESCUE 21", 0.0, 0.0)
    }
}

/// Somebody walking east along the river.
fn quinn(tick: u32) -> MapFeature {
    quinn_at(f64::from(tick), Utc::now())
}

fn quinn_at(tick: f64, time: DateTime<Utc>) -> MapFeature {
    MapFeature {
        time,
        received_at: time,
        course: Some(84.0),
        speed: Some(1.4),
        ..person(
            "ANDROID-2f1c9a7b4e0d",
            "QUINN",
            "Cyan",
            "Team Lead",
            51.50735,
            -0.12776 + tick * 0.00012,
        )
    }
}

/// The feed ticks this often, so a fix this many seconds old is half as many
/// ticks back.
const TICK_SECONDS: i64 = 2;

/// Where a demo feature has been over the last `secago` seconds, oldest first.
///
/// The two things that move have ten minutes of past, a fix every tick, on the
/// same circuit and the same walk the feed continues; the rest have only where
/// they are.
pub fn map_history(uid: &str, secago: i64) -> Vec<MapFeature> {
    let span = secago.clamp(0, 10 * 60);
    let fixes = (0..=span / TICK_SECONDS)
        .rev()
        .map(|step| step * TICK_SECONDS);
    let tick = |ago: i64| -(ago as f64) / TICK_SECONDS as f64;

    match uid {
        "ICAO-406b2f" => fixes.map(|secs| rescue_at(tick(secs), ago(secs))).collect(),
        "ANDROID-2f1c9a7b4e0d" => fixes.map(|secs| quinn_at(tick(secs), ago(secs))).collect(),
        _ => map_features()
            .into_iter()
            .filter(|feature| feature.uid == uid)
            .collect(),
    }
}

pub fn map_features() -> Vec<MapFeature> {
    let mut listed = fixtures();
    REMOVED.with(|removed| listed.retain(|feature| !removed.borrow().contains(&feature.uid)));
    // What was published over something the demo started with replaces it.
    PUBLISHED.with(|published| {
        let published = published.borrow();
        listed.retain(|feature| !published.iter().any(|over| over.uid == feature.uid));
        listed.extend(published.iter().cloned());
    });
    listed
}

/// Publishes a marker the way the server would: as the feature it becomes.
pub fn map_publish(uid: &str, draft: &PublishFeature) -> MapFeature {
    let published = MapFeature {
        uid: uid.to_string(),
        kind: draft.kind.clone(),
        how: Some(draft.how.clone().unwrap_or_else(|| "h-g-i-g-o".to_string())),
        callsign: Some(draft.callsign.clone()),
        team: None,
        role: None,
        time: Utc::now(),
        stale: draft.stale.unwrap_or_else(|| ago(-86_400)),
        received_at: Utc::now(),
        point: draft.point,
        shape: None,
        course: None,
        speed: None,
        battery: None,
        remarks: draft.remarks.clone(),
        software: None,
        sidc: draft.sidc.clone(),
        groups: draft.groups.clone(),
    };

    PUBLISHED.with(|held| {
        let mut held = held.borrow_mut();
        held.retain(|feature| feature.uid != uid);
        held.push(published.clone());
    });

    published
}

/// Forgets a marker the demo published, or refuses as the server would for
/// one it never held.
pub fn map_remove(uid: &str) -> Result<(), ApiError> {
    let published = PUBLISHED.with(|held| {
        let mut held = held.borrow_mut();
        let before = held.len();
        held.retain(|feature| feature.uid != uid);
        held.len() < before
    });
    let original = REMOVED.with(|removed| {
        let mut removed = removed.borrow_mut();
        let held = fixtures().iter().any(|feature| feature.uid == uid)
            && !removed.iter().any(|gone| gone == uid);
        if held {
            removed.push(uid.to_string());
        }
        held
    });

    match published || original {
        true => Ok(()),
        false => Err(ApiError::Server(
            "Nothing is stored under that uid.".to_string(),
        )),
    }
}

fn fixtures() -> Vec<MapFeature> {
    vec![
        quinn(0),
        person(
            "IOS-91ac4d55f207",
            "RAO",
            "Cyan",
            "Team Member",
            51.5155,
            -0.0922,
        ),
        person(
            "ANDROID-77b0e1c2",
            "OKAFOR",
            "Orange",
            "Medic",
            51.5033,
            -0.1195,
        ),
        rescue(0),
        MapFeature {
            how: Some("h-g-i-g-o".to_string()),
            remarks: Some("Two vehicles, stationary since 09:40.".to_string()),
            groups: vec!["Command".to_string()],
            ..feature("MARKER-7c19d0", "a-h-G-E-V", "CONTACT 1", 51.4975, -0.1357)
        },
        MapFeature {
            course: Some(262.0),
            speed: Some(4.1),
            groups: vec!["Maritime".to_string()],
            ..feature(
                "MMSI-235091234",
                "a-n-S-X-M",
                "THAMES CLIPPER",
                51.5079,
                -0.0877,
            )
        },
        feature("TRACK-0042", "a-u-A", "UNKNOWN 42", 51.532, -0.158),
        MapFeature {
            // Stopped reporting: stale a minute ago, so it is drawn dimmed.
            time: ago(200),
            received_at: ago(200),
            stale: ago(60),
            ..person(
                "ANDROID-0d4e",
                "MBEKI",
                "Green",
                "Team Member",
                51.4995,
                -0.1005,
            )
        },
        MapFeature {
            how: Some("h-g-i-g-o".to_string()),
            remarks: Some("Casualty collection point.".to_string()),
            ..feature("SPOT-51a0", "b-m-p-s-m", "CCP NORTH", 51.5123, -0.1312)
        },
        MapFeature {
            how: Some("h-e".to_string()),
            shape: Some(MapShape::Polygon(vec![vec![
                [-0.1105, 51.5062],
                [-0.0985, 51.5068],
                [-0.0978, 51.5012],
                [-0.1098, 51.5006],
                [-0.1105, 51.5062],
            ]])),
            ..feature("SHAPE-c41e", "u-d-f", "CORDON", 51.5037, -0.1041)
        },
        MapFeature {
            how: Some("h-e".to_string()),
            shape: Some(MapShape::LineString(vec![
                [-0.1312, 51.5123],
                [-0.1262, 51.5101],
                [-0.1204, 51.5079],
                [-0.1195, 51.5033],
            ])),
            ..feature("ROUTE-88f2", "b-m-r", "CASEVAC ROUTE", 51.5123, -0.1312)
        },
    ]
}

/// What the demo feed says on its `tick`th turn.
pub fn map_tick(tick: u32) -> Vec<MapUpdate> {
    vec![
        MapUpdate::Upsert(Box::new(rescue(tick))),
        MapUpdate::Upsert(Box::new(quinn(tick))),
    ]
}
