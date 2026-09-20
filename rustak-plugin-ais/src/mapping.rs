//! AIS on one side, [`Track`] on the other.
//!
//! Every source in this crate decodes its own wire format into the two
//! observations AIS actually carries — a [`Position`] and a [`StaticData`] —
//! and everything after that is here, so that "what a ship type means" is
//! decided once rather than once per source.
//!
//! # Two messages, one vessel
//!
//! A vessel broadcasts its position every few seconds and its *static* data —
//! name, call sign, IMO number, ship type, destination, dimensions — every few
//! minutes, in a different message. A feed that waited for both would show
//! nothing for six minutes; one that ignored the second would label every hull
//! `MMSI 244660000`. So a position is published on its own and enriched as soon
//! as the static report arrives; [`crate::vessels::Vessels`] is the cache that
//! makes that possible.
//!
//! # Sentinels
//!
//! AIS has no nulls. A heading of 511, a course of 360 and a speed of 102.3
//! knots all mean "not available", and publishing them would put a ship on the
//! map doing two hundred knots due north. [`heading`], [`course`] and
//! [`speed_mps`] are where they become [`None`].

use chrono::{DateTime, Utc};
use rustak_client::feed::{Track, TrackKind, VesselClass};

/// A true heading of 511 means the transmitter has none to give.
const HEADING_UNAVAILABLE: f64 = 511.0;

/// A course over ground of 360 means the same.
const COURSE_UNAVAILABLE: f64 = 360.0;

/// A speed over ground of 102.3 knots means the same. Anything faster is the
/// same sentinel arriving rounded.
const SPEED_UNAVAILABLE_KNOTS: f64 = 102.3;

/// One knot, in metres per second (1852 m / 3600 s).
const KNOT_MPS: f64 = 0.514_444_444_444_444_4;

/// The remark a navigational status is written under, and what
/// [`is_stationary`] reads back.
pub const STATUS_REMARK: &str = "Status";

/// The navigational statuses a vessel holds for a long time, and which
/// therefore earn the longer staleness: at anchor, moored, aground.
const STATIONARY_STATUSES: [u8; 3] = [1, 5, 6];

/// Where a vessel was, as one of its position reports said.
#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    /// The Maritime Mobile Service Identity — nine digits, and the only
    /// identifier every AIS message carries.
    pub mmsi: u32,
    /// Latitude and longitude in decimal degrees, WGS-84.
    pub position: (f64, f64),
    /// Speed over ground, in knots, before the sentinel is applied.
    pub sog_knots: Option<f64>,
    /// Course over ground, in degrees true, before the sentinel is applied.
    pub cog_deg: Option<f64>,
    /// True heading, in degrees, before the sentinel is applied.
    pub heading_deg: Option<f64>,
    /// The navigational status code (0–15). Class B reports carry none.
    pub nav_status: Option<u8>,
    /// When the vessel was there, as its source said.
    pub observed_at: DateTime<Utc>,
}

impl Position {
    /// Whether this is a position a map can show.
    ///
    /// AIS spells "no fix" as latitude 91 and longitude 181, which would
    /// otherwise be published as a vessel at the north pole.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        let (lat, lon) = self.position;

        lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && lon.abs() <= 180.0
    }
}

/// What a vessel says about itself, from a type 5 or type 24 report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StaticData {
    /// The MMSI this describes.
    pub mmsi: u32,
    /// The vessel's name, as broadcast and already trimmed of its padding.
    pub name: Option<String>,
    /// The radio call sign.
    pub call_sign: Option<String>,
    /// The IMO number, which unlike an MMSI follows a hull for its life.
    pub imo: Option<u32>,
    /// The combined ship-and-cargo type code, 0–99.
    pub ship_type: Option<u8>,
    /// Where it says it is going. Free text, and frequently a joke.
    pub destination: Option<String>,
    /// Estimated time of arrival, already rendered for a human.
    pub eta: Option<String>,
    /// The four distances that give a hull's length and beam.
    pub dimensions: Option<Dimensions>,
}

/// A hull's extent, as the four distances from its position reference point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dimensions {
    /// Metres from the reference point to the bow.
    pub to_bow: u16,
    /// Metres from the reference point to the stern.
    pub to_stern: u16,
    /// Metres from the reference point to the port side.
    pub to_port: u16,
    /// Metres from the reference point to the starboard side.
    pub to_starboard: u16,
}

impl Dimensions {
    /// Overall length, in metres.
    #[must_use]
    pub const fn length_m(self) -> u32 {
        self.to_bow as u32 + self.to_stern as u32
    }

    /// Overall beam, in metres.
    #[must_use]
    pub const fn beam_m(self) -> u32 {
        self.to_port as u32 + self.to_starboard as u32
    }

    /// Whether these dimensions say anything. A vessel that reports zeros is a
    /// vessel that did not fill the field in.
    #[must_use]
    pub const fn is_known(self) -> bool {
        self.length_m() > 0 || self.beam_m() > 0
    }
}

/// The uid a vessel appears on the map under.
///
/// Prefixed, because an MMSI on its own would collide with whatever else a
/// deployment publishes; every feed plugin owns its own prefix.
#[must_use]
pub fn id(mmsi: u32) -> String {
    format!("AIS-{mmsi}")
}

/// A true heading, or [`None`] for the sentinel and for anything off the compass.
#[must_use]
pub fn heading(raw: Option<f64>) -> Option<f64> {
    raw.filter(|value| value.is_finite() && *value >= 0.0 && *value < HEADING_UNAVAILABLE)
        .filter(|value| *value <= 359.0)
}

/// A course over ground, or [`None`] for the sentinel.
#[must_use]
pub fn course(raw: Option<f64>) -> Option<f64> {
    raw.filter(|value| value.is_finite() && *value >= 0.0 && *value < COURSE_UNAVAILABLE)
}

/// A speed in metres per second, or [`None`] for the sentinel.
#[must_use]
pub fn speed_mps(knots: Option<f64>) -> Option<f64> {
    knots
        .filter(|value| value.is_finite() && *value >= 0.0 && *value < SPEED_UNAVAILABLE_KNOTS)
        .map(|value| value * KNOT_MPS)
}

/// What kind of vessel a ship-type code describes.
///
/// The codes are the AIS combined ship-and-cargo field; the classes are what
/// [`rustak_client::feed`] turns into a CoT type. Everything not named — a
/// wing-in-ground craft, a tug, a pilot vessel, a code nobody filled in —
/// becomes [`VesselClass::Other`], which is the honest answer rather than a
/// guess that puts a dredger on the map as a warship.
#[must_use]
pub fn vessel_class(ship_type: u8) -> VesselClass {
    match ship_type {
        30 => VesselClass::Fishing,
        35 => VesselClass::Military,
        36 | 37 => VesselClass::Leisure,
        55 => VesselClass::LawEnforcement,
        60..=89 => VesselClass::Merchant,
        _ => VesselClass::Other,
    }
}

/// What a ship-type code is called, for the remarks.
#[must_use]
pub fn ship_type_word(code: u8) -> &'static str {
    match code {
        20..=29 => "wing in ground",
        30 => "fishing",
        31 | 32 => "towing",
        33 => "dredging or underwater operations",
        34 => "diving operations",
        35 => "military operations",
        36 => "sailing",
        37 => "pleasure craft",
        40..=49 => "high-speed craft",
        50 => "pilot vessel",
        51 => "search and rescue",
        52 => "tug",
        53 => "port tender",
        54 => "anti-pollution equipment",
        55 => "law enforcement",
        58 => "medical transport",
        59 => "non-combatant",
        60..=69 => "passenger",
        70..=79 => "cargo",
        80..=89 => "tanker",
        90..=99 => "other",
        _ => "unknown",
    }
}

/// What a navigational-status code is called, for the remarks.
#[must_use]
pub fn nav_status_word(code: u8) -> &'static str {
    match code {
        0 => "under way using engine",
        1 => "at anchor",
        2 => "not under command",
        3 => "restricted manoeuvrability",
        4 => "constrained by draught",
        5 => "moored",
        6 => "aground",
        7 => "engaged in fishing",
        8 => "under way sailing",
        11 => "under tow astern",
        12 => "pushing ahead or towing alongside",
        14 => "AIS-SART, MOB-AIS or EPIRB-AIS is active",
        _ => "not defined",
    }
}

/// Whether a navigational status is one a vessel holds for a long time.
#[must_use]
pub fn is_stationary_status(code: u8) -> bool {
    STATIONARY_STATUSES.contains(&code)
}

/// Whether a published track is one of those vessels.
///
/// Read back out of the remarks rather than carried on the [`Track`], because
/// a navigational status is a property of AIS and not of every feed — putting
/// one on the shared model would be putting a vessel's vocabulary in an
/// aircraft's type. The remark is written by [`track`] a few lines below, and
/// the round trip is asserted in this module's tests.
#[must_use]
pub fn is_stationary(track: &Track) -> bool {
    track.remarks.iter().any(|(key, value)| {
        key == STATUS_REMARK
            && STATIONARY_STATUSES
                .iter()
                .any(|code| nav_status_word(*code) == value)
    })
}

/// Builds the track a position (and whatever static data has arrived for the
/// same vessel) describes.
///
/// `source` is what the `Source` remark says: the upstream this observation
/// came from, so that an operator looking at a hull knows whether it was heard
/// on the roof or downloaded.
#[must_use]
pub fn track(position: &Position, statics: Option<&StaticData>, source: &str) -> Track {
    let ship_type = statics.and_then(|statics| statics.ship_type);
    let kind = TrackKind::Vessel(ship_type.map_or(VesselClass::Other, vessel_class));
    let name = statics.and_then(|statics| statics.name.as_deref());

    let mut track = Track::new(
        id(position.mmsi),
        kind,
        position.position,
        position.observed_at,
    )
    .with_callsign(name.map_or_else(|| format!("MMSI {}", position.mmsi), ToOwned::to_owned))
    .with_remark("MMSI", position.mmsi.to_string());

    if let Some(speed) = speed_mps(position.sog_knots) {
        // `with_velocity` sets both, so a course we do not have would become
        // due north; the bearing falls back to the heading in the model.
        track.speed_mps = Some(speed);
    }

    track.course_deg = course(position.cog_deg);

    if let Some(heading) = heading(position.heading_deg) {
        track = track.with_heading_deg(heading);
    }

    track = remarks(track, position, statics, ship_type);

    track.with_remark("Source", source)
}

/// The `key: value` lines under the hull on an operator's screen.
fn remarks(
    mut track: Track,
    position: &Position,
    statics: Option<&StaticData>,
    ship_type: Option<u8>,
) -> Track {
    if let Some(call_sign) = statics.and_then(|statics| statics.call_sign.as_deref()) {
        track = track.with_remark("Call sign", call_sign);
    }

    if let Some(imo) = statics
        .and_then(|statics| statics.imo)
        .filter(|imo| *imo > 0)
    {
        track = track.with_remark("IMO", imo.to_string());
    }

    if let Some(code) = ship_type {
        track = track.with_remark("Type", format!("{code} ({})", ship_type_word(code)));
    }

    if let Some(status) = position.nav_status {
        track = track.with_remark(STATUS_REMARK, nav_status_word(status));
    }

    if let Some(destination) = statics.and_then(|statics| statics.destination.as_deref()) {
        track = track.with_remark("Destination", destination);
    }

    if let Some(eta) = statics.and_then(|statics| statics.eta.as_deref()) {
        track = track.with_remark("ETA", eta);
    }

    match statics.and_then(|statics| statics.dimensions) {
        Some(dimensions) if dimensions.is_known() => track.with_remark(
            "Length/Beam",
            format!("{} m / {} m", dimensions.length_m(), dimensions.beam_m()),
        ),
        _ => track,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mmsi: u32) -> Position {
        Position {
            mmsi,
            position: (51.9512, 4.1338),
            sog_knots: Some(12.0),
            cog_deg: Some(271.5),
            heading_deg: Some(270.0),
            nav_status: Some(0),
            observed_at: "2026-09-20T12:00:00Z".parse().expect("an instant"),
        }
    }

    fn statics() -> StaticData {
        StaticData {
            mmsi: 244660000,
            name: Some("ZEEBRUGGE".into()),
            call_sign: Some("PBZE".into()),
            imo: Some(9_312_345),
            ship_type: Some(70),
            destination: Some("ROTTERDAM".into()),
            eta: Some("09-21 06:00".into()),
            dimensions: Some(Dimensions {
                to_bow: 120,
                to_stern: 30,
                to_port: 11,
                to_starboard: 11,
            }),
        }
    }

    #[test]
    fn every_ship_type_arm_maps_to_the_class_the_brief_names() {
        // One case per arm, plus the boundaries of each range: this table is
        // the whole of what an operator sees as a symbol on the map.
        for (code, expected) in [
            (0, VesselClass::Other),
            (29, VesselClass::Other),
            (30, VesselClass::Fishing),
            (31, VesselClass::Other),
            (35, VesselClass::Military),
            (36, VesselClass::Leisure),
            (37, VesselClass::Leisure),
            (38, VesselClass::Other),
            (52, VesselClass::Other),
            (55, VesselClass::LawEnforcement),
            (59, VesselClass::Other),
            (60, VesselClass::Merchant),
            (70, VesselClass::Merchant),
            (80, VesselClass::Merchant),
            (89, VesselClass::Merchant),
            (90, VesselClass::Other),
            (99, VesselClass::Other),
        ] {
            assert_eq!(vessel_class(code), expected, "ship type {code}");
        }
    }

    #[test]
    fn a_class_becomes_the_cot_type_an_operators_map_draws() {
        use rustak_client::feed::Affiliation;

        assert_eq!(
            TrackKind::Vessel(vessel_class(70)).cot_type(Affiliation::Unknown),
            "a-u-S-X-M",
        );
        assert_eq!(
            TrackKind::Vessel(vessel_class(30)).cot_type(Affiliation::Unknown),
            "a-u-S-X-F",
        );
        assert_eq!(
            TrackKind::Vessel(vessel_class(35)).cot_type(Affiliation::Unknown),
            "a-u-S-C",
        );
    }

    #[test]
    fn the_not_available_sentinels_become_nothing_at_all() {
        assert_eq!(heading(Some(511.0)), None, "511 is 'no heading'");
        assert_eq!(heading(Some(360.0)), None, "off the compass");
        assert_eq!(heading(Some(0.0)), Some(0.0), "due north is a heading");
        assert_eq!(heading(Some(359.0)), Some(359.0));
        assert_eq!(heading(None), None);

        assert_eq!(course(Some(360.0)), None, "360 is 'no course'");
        assert_eq!(course(Some(359.9)), Some(359.9));
        assert_eq!(course(Some(0.0)), Some(0.0));
        assert_eq!(course(Some(f64::NAN)), None);

        assert_eq!(speed_mps(Some(102.3)), None, "102.3 knots is 'no speed'");
        assert_eq!(speed_mps(Some(102.4)), None, "and so is anything past it");
        assert_eq!(speed_mps(Some(-1.0)), None);
        assert_eq!(speed_mps(Some(0.0)), Some(0.0), "stopped is a speed");
        let ten = speed_mps(Some(10.0)).expect("ten knots");
        assert!((ten - 5.144_444).abs() < 1e-5, "{ten}");
    }

    #[test]
    fn a_position_with_no_fix_is_not_a_position() {
        let mut nowhere = at(1);
        nowhere.position = (91.0, 181.0);
        assert!(!nowhere.is_usable(), "91/181 is AIS for 'no fix'");

        let mut nan = at(1);
        nan.position = (f64::NAN, 0.0);
        assert!(!nan.is_usable());

        assert!(at(1).is_usable());
    }

    #[test]
    fn a_position_with_no_static_data_is_still_a_track() {
        let track = track(&at(244_660_000), None, "aisstream.io");

        assert_eq!(track.id, "AIS-244660000");
        assert_eq!(track.kind, TrackKind::Vessel(VesselClass::Other));
        assert_eq!(
            track.callsign.as_deref(),
            Some("MMSI 244660000"),
            "an MMSI is better than an empty label",
        );
        assert_eq!(
            track.remarks,
            vec![
                ("MMSI".to_string(), "244660000".to_string()),
                ("Status".to_string(), "under way using engine".to_string()),
                ("Source".to_string(), "aisstream.io".to_string()),
            ],
        );
    }

    #[test]
    fn static_data_names_the_hull_and_fills_the_remarks() {
        let track = track(&at(244_660_000), Some(&statics()), "receiver");

        assert_eq!(track.callsign.as_deref(), Some("ZEEBRUGGE"));
        assert_eq!(track.kind, TrackKind::Vessel(VesselClass::Merchant));
        assert_eq!(
            track.remarks,
            vec![
                ("MMSI".to_string(), "244660000".to_string()),
                ("Call sign".to_string(), "PBZE".to_string()),
                ("IMO".to_string(), "9312345".to_string()),
                ("Type".to_string(), "70 (cargo)".to_string()),
                ("Status".to_string(), "under way using engine".to_string()),
                ("Destination".to_string(), "ROTTERDAM".to_string()),
                ("ETA".to_string(), "09-21 06:00".to_string()),
                ("Length/Beam".to_string(), "150 m / 22 m".to_string()),
                ("Source".to_string(), "receiver".to_string()),
            ],
        );
    }

    #[test]
    fn zero_dimensions_and_a_zero_imo_are_fields_nobody_filled_in() {
        let sparse = StaticData {
            imo: Some(0),
            dimensions: Some(Dimensions::default()),
            ..statics()
        };
        let track = track(&at(1), Some(&sparse), "receiver");
        let keys: Vec<&str> = track.remarks.iter().map(|(key, _)| key.as_str()).collect();

        assert!(!keys.contains(&"IMO"), "{keys:?}");
        assert!(!keys.contains(&"Length/Beam"), "{keys:?}");
    }

    #[test]
    fn a_class_b_report_without_a_navigational_status_says_nothing_about_one() {
        let mut class_b = at(1);
        class_b.nav_status = None;
        class_b.heading_deg = Some(511.0);
        class_b.cog_deg = Some(360.0);
        class_b.sog_knots = Some(102.3);

        let track = track(&class_b, None, "receiver");

        assert!(track.remarks.iter().all(|(key, _)| key != STATUS_REMARK));
        assert_eq!(track.speed_mps, None);
        assert_eq!(track.course_deg, None);
        assert_eq!(track.heading_deg, None);
        assert_eq!(track.bearing_deg(), None);
    }

    #[test]
    fn a_heading_stands_in_when_only_the_course_is_missing() {
        let mut drifting = at(1);
        drifting.cog_deg = Some(360.0);

        let track = track(&drifting, None, "receiver");

        assert_eq!(track.course_deg, None);
        assert_eq!(track.heading_deg, Some(270.0));
        assert_eq!(track.bearing_deg(), Some(270.0));
    }

    #[test]
    fn every_navigational_status_code_has_a_word() {
        assert_eq!(nav_status_word(0), "under way using engine");
        assert_eq!(nav_status_word(1), "at anchor");
        assert_eq!(nav_status_word(5), "moored");
        assert_eq!(nav_status_word(6), "aground");
        assert_eq!(nav_status_word(15), "not defined");
        assert_eq!(nav_status_word(200), "not defined", "off the 4-bit field");
    }

    #[test]
    fn every_ship_type_code_has_a_word() {
        assert_eq!(ship_type_word(0), "unknown");
        assert_eq!(ship_type_word(19), "unknown", "the reserved 1x range");
        assert_eq!(ship_type_word(30), "fishing");
        assert_eq!(ship_type_word(55), "law enforcement");
        assert_eq!(ship_type_word(71), "cargo");
        assert_eq!(ship_type_word(81), "tanker");
    }

    #[test]
    fn a_stationary_vessel_is_one_a_published_track_can_be_asked_about() {
        // The round trip the staleness decision depends on: what `track` wrote
        // is what `is_stationary` reads.
        for code in 0..=15u8 {
            let mut vessel = at(1);
            vessel.nav_status = Some(code);

            assert_eq!(
                is_stationary(&track(&vessel, None, "receiver")),
                is_stationary_status(code),
                "navigational status {code}",
            );
        }

        let mut unknown = at(1);
        unknown.nav_status = None;
        assert!(
            !is_stationary(&track(&unknown, None, "receiver")),
            "a vessel that said nothing is not moored",
        );
    }
}
