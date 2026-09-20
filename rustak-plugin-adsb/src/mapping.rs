//! Turning what a decoder said into what goes on the map.
//!
//! Everything ADS-B-specific lives here: the emitter categories, the units, the
//! `"ground"` sentinel, and the ordered remark lines an operator reads when
//! they tap a track. [`rustak_client::feed`] takes it from there and neither
//! knows nor cares that any of this came off 1090 MHz.
//!
//! # The emitter categories
//!
//! ADS-B carries a category the aircraft sets for itself, `A0`–`D7`, and
//! OpenSky publishes the same idea as a number. Both are mapped onto the shared
//! [`TrackKind`], which is a much smaller vocabulary on purpose: the CoT type
//! decides a symbol on a map, and an operator does not need a different symbol
//! for a light aircraft and a small one.
//!
//! | Category | OpenSky | Kind |
//! |---|---|---|
//! | `A1`–`A6`, `B1`, `B4` | 2–7, 9, 12 | [`AircraftClass::CivilFixedWing`] |
//! | `A7` | 8 | [`AircraftClass::CivilRotary`] |
//! | `B2` | 10 | [`AircraftClass::LighterThanAir`] |
//! | `B6` | 14 | [`AircraftClass::Uav`] |
//! | `C1`–`C3` | 16, 17 | [`TrackKind::GroundVehicle`] |
//! | anything else | anything else | [`AircraftClass::Unknown`] |
//!
//! An `A*` category on an airframe the upstream's database marks military
//! (`dbFlags` bit 1) becomes [`AircraftClass::MilitaryFixedWing`] — including
//! `A7`, because the shared model has no military-rotary class and a military
//! helicopter drawn as a civil one is the worse of the two mistakes. OpenSky
//! publishes no military flag, so nothing from it is ever flipped.

use chrono::{DateTime, Utc};
use rustak_client::feed::{AircraftClass, Track, TrackKind};

use crate::wire::{Aircraft, StateVector};

/// Metres in a foot, exactly.
pub const FEET_TO_METRES: f64 = 0.3048;

/// Metres per second in a knot, exactly (1852 m in a nautical mile).
pub const KNOTS_TO_MPS: f64 = 1_852.0 / 3_600.0;

/// Feet per minute in a metre per second, for OpenSky's vertical rate.
pub const MPS_TO_FEET_PER_MINUTE: f64 = 60.0 / FEET_TO_METRES;

/// How old a position may be and still be worth drawing, in seconds.
///
/// An aircraft at cruise covers fifteen kilometres in a minute, so a position
/// older than this is not a track that has gone quiet — it is an aeroplane
/// drawn somewhere it demonstrably is not.
pub const MAX_POSITION_AGE_S: f64 = 60.0;

/// The uid prefix every track from this plugin carries.
const PREFIX: &str = "ADSB-";

/// The [`TrackKind`] a `readsb` emitter category means.
#[must_use]
pub fn kind_from_category(category: Option<&str>, military: bool) -> TrackKind {
    let category = category.unwrap_or_default().trim().to_ascii_uppercase();

    // Military first, because it overrides every `A*` arm below and reading it
    // afterwards would mean spelling the arms out twice.
    if military && category.starts_with('A') {
        return TrackKind::Aircraft(AircraftClass::MilitaryFixedWing);
    }

    match category.as_str() {
        "A7" => TrackKind::Aircraft(AircraftClass::CivilRotary),
        "B2" => TrackKind::Aircraft(AircraftClass::LighterThanAir),
        "B6" => TrackKind::Aircraft(AircraftClass::Uav),
        "A1" | "A2" | "A3" | "A4" | "A5" | "A6" | "B1" | "B4" => {
            TrackKind::Aircraft(AircraftClass::CivilFixedWing)
        }
        "C1" | "C2" | "C3" => TrackKind::GroundVehicle,
        _ => TrackKind::Aircraft(AircraftClass::Unknown),
    }
}

/// The [`TrackKind`] an OpenSky category number means.
#[must_use]
pub fn kind_from_opensky_category(category: Option<u8>) -> TrackKind {
    match category {
        Some(8) => TrackKind::Aircraft(AircraftClass::CivilRotary),
        Some(10) => TrackKind::Aircraft(AircraftClass::LighterThanAir),
        Some(14) => TrackKind::Aircraft(AircraftClass::Uav),
        Some(2..=7 | 9 | 12) => TrackKind::Aircraft(AircraftClass::CivilFixedWing),
        Some(16 | 17) => TrackKind::GroundVehicle,
        _ => TrackKind::Aircraft(AircraftClass::Unknown),
    }
}

/// One aircraft as a track, or [`None`] for one there is nothing to draw.
///
/// Skipped: an aircraft with no position (a Mode S return with an altitude and
/// nothing else is most of a busy receiver's list) and one whose position is
/// older than [`MAX_POSITION_AGE_S`].
#[must_use]
pub fn track_from_aircraft(aircraft: &Aircraft, now: DateTime<Utc>) -> Option<Track> {
    let (lat, lon) = (aircraft.lat?, aircraft.lon?);
    let age = aircraft.seen_pos.unwrap_or(0.0);

    if !(0.0..=MAX_POSITION_AGE_S).contains(&age) {
        return None;
    }

    let hex = aircraft.hex.as_deref()?.trim().to_ascii_lowercase();
    let on_ground = aircraft
        .alt_baro
        .as_ref()
        .is_some_and(super::wire::AltBaro::on_ground);
    let barometric = aircraft
        .alt_baro
        .as_ref()
        .and_then(super::wire::AltBaro::feet);
    let kind = kind_from_category(aircraft.category.as_deref(), aircraft.military());

    let mut track = Track::new(
        format!("{PREFIX}{hex}"),
        kind,
        (lat, lon),
        observed(now, age),
    )
    .with_on_ground(on_ground)
    .with_callsign(callsign(aircraft, &hex));

    // Geometric first: it is already height above the ellipsoid, which is what
    // CoT's `hae` means. A barometric altitude is a pressure reading standing
    // in for one, so it is converted and the remarks say so.
    if let Some(feet) = aircraft.alt_geom.or(barometric) {
        track = track.with_altitude_hae_m(feet * FEET_TO_METRES);
    }

    if let (Some(knots), Some(course)) = (aircraft.gs, aircraft.track) {
        track = track.with_velocity(knots * KNOTS_TO_MPS, course);
    }

    if let Some(heading) = aircraft.true_heading {
        track = track.with_heading_deg(heading);
    }

    for (key, value) in remarks(aircraft, on_ground, barometric, age) {
        track = track.with_remark(key, value);
    }

    Some(track)
}

/// One OpenSky state vector as a track, or [`None`] when there is nothing to
/// draw.
#[must_use]
pub fn track_from_state(state: &StateVector, now: DateTime<Utc>) -> Option<Track> {
    let (lat, lon) = (state.latitude?, state.longitude?);
    let age = state
        .time_position
        .map_or(0.0, |at| (now.timestamp() - at).max(0) as f64);

    if age > MAX_POSITION_AGE_S {
        return None;
    }

    let hex = state.icao24.as_deref()?.trim().to_ascii_lowercase();
    let name = state
        .callsign
        .as_deref()
        .map(str::trim)
        .filter(|callsign| !callsign.is_empty())
        .unwrap_or(&hex)
        .to_string();

    let mut track = Track::new(
        format!("{PREFIX}{hex}"),
        kind_from_opensky_category(state.category),
        (lat, lon),
        observed(now, age),
    )
    .with_on_ground(state.on_ground)
    .with_callsign(name);

    // OpenSky reports both altitudes in metres already; `geo_altitude` is the
    // ellipsoidal one, so it is the `hae` and the barometric is the fallback.
    if let Some(metres) = state.geo_altitude_m.or(state.baro_altitude_m) {
        track = track.with_altitude_hae_m(metres);
    }

    if let (Some(speed), Some(course)) = (state.velocity_mps, state.true_track) {
        track = track.with_velocity(speed, course);
    }

    for (key, value) in opensky_remarks(state, age) {
        track = track.with_remark(key, value);
    }

    Some(track)
}

/// When the observation was made, from how old the upstream says it is.
///
/// Against *our* clock rather than the receiver's: a receiver with no NTP is
/// common, and the publisher measures staleness against wall-clock now.
fn observed(now: DateTime<Utc>, age_seconds: f64) -> DateTime<Utc> {
    now - chrono::Duration::milliseconds((age_seconds * 1_000.0) as i64)
}

/// What the map shows: the callsign the crew set, else the registration, else
/// the address — never nothing, because a track with no callsign shows its uid.
fn callsign(aircraft: &Aircraft, hex: &str) -> String {
    aircraft
        .flight
        .as_deref()
        .map(str::trim)
        .filter(|flight| !flight.is_empty())
        .or(aircraft.r.as_deref())
        .unwrap_or(hex)
        .to_string()
}

/// The ordered `key: value` lines an operator reads when they tap the track.
fn remarks(
    aircraft: &Aircraft,
    on_ground: bool,
    barometric: Option<f64>,
    age: f64,
) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = Vec::new();
    let mut push = |key: &str, value: String| lines.push((key.to_string(), value));

    if let Some(hex) = &aircraft.hex {
        push("ICAO", hex.trim().to_ascii_lowercase());
    }
    if let Some(registration) = &aircraft.r {
        push("Registration", registration.clone());
    }
    match (&aircraft.t, &aircraft.desc) {
        (Some(code), Some(description)) => push("Type", format!("{code} ({description})")),
        (Some(code), None) => push("Type", code.clone()),
        (None, Some(description)) => push("Type", description.clone()),
        (None, None) => {}
    }
    if let Some(squawk) = &aircraft.squawk {
        push("Squawk", squawk.clone());
    }
    if let Some(altitude) = altitude_line(on_ground, barometric, aircraft.alt_geom) {
        push("Altitude", altitude);
    }
    if let Some(rate) = aircraft.geom_rate.or(aircraft.baro_rate) {
        let source = if aircraft.geom_rate.is_some() {
            "geometric"
        } else {
            "barometric"
        };
        push("Vertical rate", format!("{rate:+.0} ft/min {source}"));
    }
    if let Some(category) = &aircraft.category {
        push("Category", category_line(category));
    }
    if let Some(emergency) = emergency(aircraft.emergency.as_deref()) {
        push("Emergency", emergency.to_string());
    }
    if let Some(kind) = &aircraft.kind {
        push("Source", source_line(kind));
    }
    push("Seen", format!("{age:.1} s ago"));

    lines
}

/// The same lines for an OpenSky state vector, which carries less.
fn opensky_remarks(state: &StateVector, age: f64) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = Vec::new();
    let mut push = |key: &str, value: String| lines.push((key.to_string(), value));

    if let Some(hex) = &state.icao24 {
        push("ICAO", hex.trim().to_ascii_lowercase());
    }
    if let Some(country) = &state.origin_country {
        push("Country", country.clone());
    }
    if let Some(squawk) = &state.squawk {
        push("Squawk", squawk.clone());
    }
    let feet = |metres: f64| metres / FEET_TO_METRES;
    if let Some(altitude) = altitude_line(
        state.on_ground,
        state.baro_altitude_m.map(feet),
        state.geo_altitude_m.map(feet),
    ) {
        push("Altitude", altitude);
    }
    if let Some(rate) = state.vertical_rate_mps {
        push(
            "Vertical rate",
            format!("{:+.0} ft/min", rate * MPS_TO_FEET_PER_MINUTE),
        );
    }
    if let Some(category) = state.category {
        push("Category", format!("OpenSky {category}"));
    }
    push("Source", "OpenSky".to_string());
    push("Seen", format!("{age:.1} s ago"));

    lines
}

/// `Altitude`, which says which of the two readings it had.
fn altitude_line(
    on_ground: bool,
    barometric: Option<f64>,
    geometric: Option<f64>,
) -> Option<String> {
    if on_ground {
        return Some("on the ground".to_string());
    }

    match (barometric, geometric) {
        (Some(baro), Some(geom)) => {
            Some(format!("{baro:.0} ft barometric, {geom:.0} ft geometric"))
        }
        (Some(baro), None) => Some(format!("{baro:.0} ft barometric")),
        (None, Some(geom)) => Some(format!("{geom:.0} ft geometric")),
        (None, None) => None,
    }
}

/// An emergency worth showing, which is any value but `none`.
fn emergency(reported: Option<&str>) -> Option<&str> {
    reported
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("none"))
}

/// The category code with what it means beside it, where we know.
fn category_line(category: &str) -> String {
    let label = match category.trim().to_ascii_uppercase().as_str() {
        "A1" => "light",
        "A2" => "small",
        "A3" => "large",
        "A4" => "high-vortex large",
        "A5" => "heavy",
        "A6" => "high performance",
        "A7" => "rotorcraft",
        "B1" => "glider or sailplane",
        "B2" => "lighter-than-air",
        "B3" => "parachutist",
        "B4" => "ultralight",
        "B6" => "unmanned",
        "B7" => "space vehicle",
        "C1" => "surface emergency vehicle",
        "C2" => "surface service vehicle",
        "C3" => "point obstacle",
        _ => return category.to_string(),
    };

    format!("{category} ({label})")
}

/// How the position was heard, in words rather than in `readsb`'s vocabulary.
fn source_line(kind: &str) -> String {
    let lowered = kind.to_ascii_lowercase();

    match () {
        () if lowered.starts_with("adsb") => "ADS-B",
        () if lowered.starts_with("adsr") => "ADS-R",
        () if lowered.starts_with("tisb") => "TIS-B",
        () if lowered.starts_with("mlat") => "MLAT",
        () if lowered.starts_with("mode_s") || lowered.starts_with("modes") => "Mode S",
        () => return kind.to_string(),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{AltBaro, OpenSkyStates, Snapshot};

    const READSB: &str = include_str!("../tests/fixtures/readsb.json");

    fn fixture() -> Vec<Aircraft> {
        serde_json::from_str::<Snapshot>(READSB)
            .expect("the fixture parses")
            .into_aircraft()
    }

    fn remark<'a>(track: &'a Track, key: &str) -> Option<&'a str> {
        track
            .remarks
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn every_category_arm_maps_the_way_the_table_says() {
        for (category, expected) in [
            ("A1", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("A2", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("A3", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("A4", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("A5", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("A6", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("B1", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("B4", TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            ("A7", TrackKind::Aircraft(AircraftClass::CivilRotary)),
            ("B2", TrackKind::Aircraft(AircraftClass::LighterThanAir)),
            ("B6", TrackKind::Aircraft(AircraftClass::Uav)),
            ("C1", TrackKind::GroundVehicle),
            ("C2", TrackKind::GroundVehicle),
            ("C3", TrackKind::GroundVehicle),
            ("A0", TrackKind::Aircraft(AircraftClass::Unknown)),
            ("B0", TrackKind::Aircraft(AircraftClass::Unknown)),
            ("B3", TrackKind::Aircraft(AircraftClass::Unknown)),
            ("D7", TrackKind::Aircraft(AircraftClass::Unknown)),
            ("", TrackKind::Aircraft(AircraftClass::Unknown)),
        ] {
            assert_eq!(
                kind_from_category(Some(category), false),
                expected,
                "category {category}",
            );
        }

        assert_eq!(
            kind_from_category(None, false),
            TrackKind::Aircraft(AircraftClass::Unknown),
            "an aircraft that set no category is not a claim about what it is",
        );
    }

    #[test]
    fn the_military_flag_flips_every_a_category_and_nothing_else() {
        for category in ["A1", "A4", "A7", "a5"] {
            assert_eq!(
                kind_from_category(Some(category), true),
                TrackKind::Aircraft(AircraftClass::MilitaryFixedWing),
                "category {category}",
            );
        }

        assert_eq!(
            kind_from_category(Some("C2"), true),
            TrackKind::GroundVehicle,
            "a military airfield's tug is still a tug",
        );
        assert_eq!(
            kind_from_category(Some("B6"), true),
            TrackKind::Aircraft(AircraftClass::Uav),
        );
    }

    #[test]
    fn every_opensky_category_arm_maps_the_way_the_table_says() {
        for (category, expected) in [
            (2, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (3, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (4, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (5, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (6, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (7, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (9, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (12, TrackKind::Aircraft(AircraftClass::CivilFixedWing)),
            (8, TrackKind::Aircraft(AircraftClass::CivilRotary)),
            (10, TrackKind::Aircraft(AircraftClass::LighterThanAir)),
            (14, TrackKind::Aircraft(AircraftClass::Uav)),
            (16, TrackKind::GroundVehicle),
            (17, TrackKind::GroundVehicle),
            (0, TrackKind::Aircraft(AircraftClass::Unknown)),
            (1, TrackKind::Aircraft(AircraftClass::Unknown)),
            (11, TrackKind::Aircraft(AircraftClass::Unknown)),
            (15, TrackKind::Aircraft(AircraftClass::Unknown)),
            (19, TrackKind::Aircraft(AircraftClass::Unknown)),
        ] {
            assert_eq!(
                kind_from_opensky_category(Some(category)),
                expected,
                "OpenSky category {category}",
            );
        }

        assert_eq!(
            kind_from_opensky_category(None),
            TrackKind::Aircraft(AircraftClass::Unknown),
        );
    }

    #[test]
    fn an_airliner_becomes_the_track_an_operator_expects() {
        let now = Utc::now();
        let track = track_from_aircraft(&fixture()[0], now).expect("it has a position");

        assert_eq!(track.id, "ADSB-3c6444");
        assert_eq!(
            track.kind,
            TrackKind::Aircraft(AircraftClass::CivilFixedWing)
        );
        assert_eq!(track.callsign.as_deref(), Some("BAW117"));
        assert_eq!(track.position, (51.4602, -0.3947));
        assert!(!track.on_ground);

        // 3225 ft geometric is the height above the ellipsoid; the barometric
        // reading is a pressure altitude and is not used for `hae`.
        let hae = track.altitude_hae_m.expect("a geometric altitude");
        assert!((hae - 982.98).abs() < 0.01, "3225 ft in metres, got {hae}");

        let speed = track.speed_mps.expect("a ground speed");
        assert!(
            (speed - 127.376).abs() < 0.001,
            "247.6 kt in m/s, got {speed}"
        );
        assert_eq!(track.course_deg, Some(88.0));
        assert_eq!(track.heading_deg, Some(86.2));
    }

    #[test]
    fn the_remarks_are_the_ordered_lines_the_brief_asks_for() {
        let track = track_from_aircraft(&fixture()[0], Utc::now()).expect("a track");
        let keys: Vec<&str> = track.remarks.iter().map(|(key, _)| key.as_str()).collect();

        assert_eq!(
            keys,
            [
                "ICAO",
                "Registration",
                "Type",
                "Squawk",
                "Altitude",
                "Vertical rate",
                "Category",
                "Source",
                "Seen",
            ],
            "no Emergency line, because this one reported `none`",
        );

        assert_eq!(remark(&track, "ICAO"), Some("3c6444"));
        assert_eq!(remark(&track, "Registration"), Some("G-XLEA"));
        assert_eq!(remark(&track, "Type"), Some("A388 (AIRBUS A-380-800)"));
        assert_eq!(remark(&track, "Squawk"), Some("5271"));
        assert_eq!(
            remark(&track, "Altitude"),
            Some("2675 ft barometric, 3225 ft geometric"),
        );
        assert_eq!(
            remark(&track, "Vertical rate"),
            Some("+2592 ft/min geometric")
        );
        assert_eq!(remark(&track, "Category"), Some("A5 (heavy)"));
        assert_eq!(remark(&track, "Source"), Some("ADS-B"));
        assert_eq!(remark(&track, "Seen"), Some("0.1 s ago"));
    }

    #[test]
    fn an_aircraft_with_only_a_barometric_altitude_says_so() {
        let track = track_from_aircraft(&fixture()[1], Utc::now()).expect("a track");

        assert_eq!(track.kind, TrackKind::Aircraft(AircraftClass::CivilRotary));
        assert_eq!(track.callsign.as_deref(), Some("HLE21"));
        assert_eq!(remark(&track, "Altitude"), Some("1300 ft barometric"));
        assert_eq!(remark(&track, "Source"), Some("MLAT"));

        let hae = track.altitude_hae_m.expect("the barometric stands in");
        assert!((hae - 396.24).abs() < 0.01, "{hae}");
    }

    #[test]
    fn a_military_airframe_is_a_military_track() {
        let track = track_from_aircraft(&fixture()[2], Utc::now()).expect("a track");

        assert_eq!(
            track.kind,
            TrackKind::Aircraft(AircraftClass::MilitaryFixedWing),
            "dbFlags bit 1 over category A4",
        );
        assert_eq!(track.callsign.as_deref(), Some("RRR7231"));
    }

    #[test]
    fn the_ground_sentinel_becomes_an_on_ground_track_with_no_altitude() {
        let track = track_from_aircraft(&fixture()[3], Utc::now()).expect("a track");

        assert_eq!(track.kind, TrackKind::GroundVehicle);
        assert!(track.on_ground);
        assert_eq!(
            track.altitude_hae_m, None,
            "`ground` is not a number of feet"
        );
        assert_eq!(remark(&track, "Altitude"), Some("on the ground"));
        assert_eq!(
            remark(&track, "Category"),
            Some("C2 (surface service vehicle)")
        );
    }

    #[test]
    fn a_non_icao_address_keeps_its_tilde_and_its_emergency_is_shown() {
        let track = track_from_aircraft(&fixture()[4], Utc::now()).expect("a track");

        assert_eq!(track.id, "ADSB-~a1b2c3", "the ~ is part of the address");
        assert_eq!(remark(&track, "ICAO"), Some("~a1b2c3"));
        assert_eq!(remark(&track, "Emergency"), Some("general"));
        assert_eq!(remark(&track, "Source"), Some("TIS-B"));
        assert_eq!(
            track.callsign.as_deref(),
            Some("~a1b2c3"),
            "no callsign and no registration leaves the address",
        );
    }

    #[test]
    fn a_stale_position_and_a_missing_one_are_both_skipped() {
        let now = Utc::now();

        assert!(
            track_from_aircraft(&fixture()[5], now).is_none(),
            "seen_pos 120 s is an aeroplane drawn 30 km from where it is",
        );
        assert!(
            track_from_aircraft(&fixture()[6], now).is_none(),
            "a Mode S return with no position is not a track",
        );
    }

    #[test]
    fn the_whole_fixture_yields_exactly_the_five_drawable_aircraft() {
        let now = Utc::now();
        let tracks: Vec<Track> = fixture()
            .iter()
            .filter_map(|aircraft| track_from_aircraft(aircraft, now))
            .collect();

        assert_eq!(tracks.len(), 5);
        assert!(tracks.iter().all(|track| track.id.starts_with("ADSB-")));
    }

    #[test]
    fn an_upper_case_address_is_lower_cased_into_the_uid() {
        let aircraft = Aircraft {
            hex: Some("3C6444".to_string()),
            lat: Some(51.0),
            lon: Some(-0.1),
            ..Aircraft::default()
        };

        let track = track_from_aircraft(&aircraft, Utc::now()).expect("a track");

        assert_eq!(track.id, "ADSB-3c6444");
    }

    #[test]
    fn a_registration_stands_in_for_a_missing_callsign() {
        let aircraft = Aircraft {
            hex: Some("400f1a".to_string()),
            flight: Some("        ".to_string()),
            r: Some("G-ABCD".to_string()),
            lat: Some(51.0),
            lon: Some(-0.1),
            ..Aircraft::default()
        };

        let track = track_from_aircraft(&aircraft, Utc::now()).expect("a track");

        assert_eq!(track.callsign.as_deref(), Some("G-ABCD"));
    }

    #[test]
    fn a_barometric_climb_rate_is_labelled_as_one() {
        let aircraft = Aircraft {
            hex: Some("400f1a".to_string()),
            baro_rate: Some(-1_216.0),
            alt_baro: Some(AltBaro::Feet(8_000.0)),
            lat: Some(51.0),
            lon: Some(-0.1),
            ..Aircraft::default()
        };

        let track = track_from_aircraft(&aircraft, Utc::now()).expect("a track");

        assert_eq!(
            remark(&track, "Vertical rate"),
            Some("-1216 ft/min barometric")
        );
    }

    #[test]
    fn an_opensky_state_vector_becomes_the_same_kind_of_track() {
        let states: OpenSkyStates =
            serde_json::from_str(include_str!("../tests/fixtures/opensky.json"))
                .expect("the fixture parses");
        let vectors = states.states.expect("states are present");
        let now = DateTime::from_timestamp(states.time, 0).expect("a time");

        let airliner = track_from_state(&vectors[0], now).expect("a track");

        assert_eq!(airliner.id, "ADSB-3c6444");
        assert_eq!(airliner.callsign.as_deref(), Some("BAW117"));
        assert_eq!(
            airliner.kind,
            TrackKind::Aircraft(AircraftClass::CivilFixedWing),
        );
        assert_eq!(
            airliner.altitude_hae_m,
            Some(801.6),
            "geo_altitude is already metres above the ellipsoid",
        );
        assert_eq!(airliner.speed_mps, Some(77.2), "OpenSky speaks m/s already");
        assert_eq!(airliner.course_deg, Some(88.0));
        assert_eq!(remark(&airliner, "Country"), Some("United Kingdom"));
        assert_eq!(remark(&airliner, "Source"), Some("OpenSky"));
        assert_eq!(remark(&airliner, "Vertical rate"), Some("+492 ft/min"));
        assert_eq!(
            remark(&airliner, "Altitude"),
            Some("2500 ft barometric, 2630 ft geometric"),
        );

        let helicopter = track_from_state(&vectors[1], now).expect("a track");
        assert_eq!(
            helicopter.kind,
            TrackKind::Aircraft(AircraftClass::CivilRotary),
        );
        assert!(helicopter.on_ground);
        assert_eq!(remark(&helicopter, "Altitude"), Some("on the ground"));

        assert!(
            track_from_state(&vectors[2], now).is_none(),
            "a state vector with no position is not a track",
        );
        assert_eq!(
            vectors.len(),
            4,
            "the fourth is the stale one the age test uses",
        );
        assert!(
            track_from_state(&vectors[3], now).is_none(),
            "a position reported four minutes ago is not where the aircraft is",
        );
    }
}
