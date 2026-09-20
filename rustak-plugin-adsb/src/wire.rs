//! The JSON these upstreams speak, and nothing else.
//!
//! Three of the four sources speak the same document: `readsb` writes
//! `aircraft.json`, and every public aggregator that pools `readsb` receivers
//! serves the same objects back under an array key of its own choosing
//! (`aircraft` for a receiver and for adsb.fi, `ac` for adsb.lol and
//! airplanes.live). [`Snapshot`] therefore accepts both keys and the sources
//! differ only in the URL they fetch. OpenSky is the odd one out: its state
//! vectors are positional arrays, which [`StateVector`] turns into named
//! fields.
//!
//! # Nothing here refuses an unknown key
//!
//! Every other configuration type in rustak uses `deny_unknown_fields`, because
//! an unrecognised key in a file an operator wrote is a typo. These are not
//! files an operator wrote: they are documents somebody else's decoder
//! generates, and a `readsb` release that adds a field must not stop this
//! plugin reading the fields it already had. Every field is therefore optional
//! and unknown ones are ignored.

use serde::Deserialize;

/// One `aircraft.json`-shaped document, whichever upstream served it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Snapshot {
    /// The receiver's clock when the snapshot was taken, in Unix seconds.
    /// Carried by every source and used to age `seen_pos` against the
    /// upstream's own clock rather than ours.
    #[serde(default)]
    pub now: Option<f64>,

    /// `readsb`'s key, which adsb.fi also uses.
    #[serde(default)]
    pub aircraft: Vec<Aircraft>,

    /// adsb.lol's and airplanes.live's key for the same array.
    #[serde(default)]
    pub ac: Vec<Aircraft>,
}

impl Snapshot {
    /// The aircraft, from whichever key carried them.
    ///
    /// A document with neither key is an empty sky rather than an error: an
    /// aggregator answers a quiet circle with an empty array, and a receiver
    /// that has just started answers with nothing at all.
    #[must_use]
    pub fn into_aircraft(self) -> Vec<Aircraft> {
        if self.aircraft.is_empty() {
            self.ac
        } else {
            self.aircraft
        }
    }
}

/// One aircraft as `readsb` reports it.
///
/// Field names are the wire's, not ours, because this type's whole job is to
/// be the shape of somebody else's document; [`crate::mapping`] is where they
/// become a [`Track`](rustak_client::feed::Track).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Aircraft {
    /// The 24-bit ICAO address in hexadecimal. A leading `~` marks an address
    /// that is not a real ICAO one (TIS-B and ADS-R targets), which is kept:
    /// two aircraft may share the digits after it.
    #[serde(default)]
    pub hex: Option<String>,

    /// How the position was heard: `adsb_icao`, `mlat`, `tisb_icao`, `mode_s`
    /// and friends.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,

    /// The callsign the crew set, space-padded to eight characters.
    #[serde(default)]
    pub flight: Option<String>,

    /// Registration, from the upstream's own airframe database.
    #[serde(default)]
    pub r: Option<String>,

    /// ICAO type code: `A320`, `B738`, `EC35`.
    #[serde(default)]
    pub t: Option<String>,

    /// The type code spelled out: `AIRBUS A-320`.
    #[serde(default)]
    pub desc: Option<String>,

    /// Barometric altitude in feet, **or the string `"ground"`**.
    #[serde(default)]
    pub alt_baro: Option<AltBaro>,

    /// Geometric altitude in feet above the WGS-84 ellipsoid — the HAE.
    #[serde(default)]
    pub alt_geom: Option<f64>,

    /// Ground speed in knots.
    #[serde(default)]
    pub gs: Option<f64>,

    /// Ground track in degrees true.
    #[serde(default)]
    pub track: Option<f64>,

    /// Heading in degrees true, when the aircraft reports one.
    #[serde(default)]
    pub true_heading: Option<f64>,

    /// Barometric rate of climb, feet per minute.
    #[serde(default)]
    pub baro_rate: Option<f64>,

    /// Geometric rate of climb, feet per minute.
    #[serde(default)]
    pub geom_rate: Option<f64>,

    /// The Mode A code, as four octal digits in a string.
    #[serde(default)]
    pub squawk: Option<String>,

    /// `none` when nothing is wrong, and one of the Mode A emergencies
    /// otherwise.
    #[serde(default)]
    pub emergency: Option<String>,

    /// The ADS-B emitter category, `A0`–`D7`.
    #[serde(default)]
    pub category: Option<String>,

    /// Latitude, decimal degrees.
    #[serde(default)]
    pub lat: Option<f64>,

    /// Longitude, decimal degrees.
    #[serde(default)]
    pub lon: Option<f64>,

    /// Seconds since the position was last updated. A stale position is worse
    /// than none: it is an aircraft drawn where it was a minute ago.
    #[serde(default)]
    pub seen_pos: Option<f64>,

    /// Seconds since any message at all was heard from this aircraft.
    #[serde(default)]
    pub seen: Option<f64>,

    /// Database flags; bit 1 marks a military airframe.
    #[serde(default, rename = "dbFlags")]
    pub db_flags: Option<u64>,
}

impl Aircraft {
    /// Whether the upstream's airframe database calls this one military.
    #[must_use]
    pub fn military(&self) -> bool {
        self.db_flags.is_some_and(|flags| flags & 1 == 1)
    }
}

/// `alt_baro`, which is a number of feet or the word `"ground"`.
///
/// The sentinel is the whole reason this is not an `Option<f64>`: an aircraft
/// on a taxiway reports `"ground"` rather than an altitude, and a parser that
/// refused the document over it would drop every aircraft on the field.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AltBaro {
    /// Feet above the 1013.25 hPa datum.
    Feet(f64),
    /// The sentinel, which is `"ground"` in every decoder we have seen.
    Sentinel(String),
}

impl AltBaro {
    /// The barometric altitude in feet, or [`None`] for the sentinel.
    #[must_use]
    pub fn feet(&self) -> Option<f64> {
        match self {
            Self::Feet(feet) => Some(*feet),
            Self::Sentinel(_) => None,
        }
    }

    /// Whether this is the `"ground"` sentinel.
    #[must_use]
    pub fn on_ground(&self) -> bool {
        matches!(self, Self::Sentinel(text) if text.eq_ignore_ascii_case("ground"))
    }
}

/// OpenSky's `GET /api/states/all` response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct OpenSkyStates {
    /// The server's clock when the snapshot was taken, Unix seconds.
    #[serde(default)]
    pub time: i64,

    /// The state vectors, which is `null` rather than `[]` for an empty box.
    #[serde(default)]
    pub states: Option<Vec<StateVector>>,
}

/// One OpenSky state vector, which crosses the wire as a positional array.
///
/// Every element is nullable, so every field here is an [`Option`]. The indices
/// are OpenSky's published order; `category` (17) is present only when the
/// request carried `extended=1`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "Vec<serde_json::Value>")]
pub struct StateVector {
    /// The 24-bit ICAO address in lower-case hexadecimal (index 0).
    pub icao24: Option<String>,
    /// The callsign, space-padded (index 1).
    pub callsign: Option<String>,
    /// The country the address is allocated to (index 2).
    pub origin_country: Option<String>,
    /// When the position was last updated, Unix seconds (index 3).
    pub time_position: Option<i64>,
    /// Longitude, decimal degrees (index 5).
    pub longitude: Option<f64>,
    /// Latitude, decimal degrees (index 6).
    pub latitude: Option<f64>,
    /// Barometric altitude in **metres** (index 7).
    pub baro_altitude_m: Option<f64>,
    /// Whether the aircraft is on the ground (index 8).
    pub on_ground: bool,
    /// Ground speed in **metres per second** (index 9).
    pub velocity_mps: Option<f64>,
    /// Ground track, degrees true (index 10).
    pub true_track: Option<f64>,
    /// Vertical rate in **metres per second** (index 11).
    pub vertical_rate_mps: Option<f64>,
    /// Geometric altitude in **metres** (index 13) — the HAE.
    pub geo_altitude_m: Option<f64>,
    /// The Mode A code (index 14).
    pub squawk: Option<String>,
    /// The emitter category, 0–20 (index 17, `extended=1` only).
    pub category: Option<u8>,
}

impl From<Vec<serde_json::Value>> for StateVector {
    /// Reads the positional array into named fields, treating a missing
    /// element and a `null` one alike.
    fn from(row: Vec<serde_json::Value>) -> Self {
        let at = |index: usize| row.get(index).filter(|value| !value.is_null());
        let text = |index: usize| {
            at(index)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let number = |index: usize| at(index).and_then(serde_json::Value::as_f64);

        Self {
            icao24: text(0),
            callsign: text(1),
            origin_country: text(2),
            time_position: at(3).and_then(serde_json::Value::as_i64),
            longitude: number(5),
            latitude: number(6),
            baro_altitude_m: number(7),
            on_ground: at(8).and_then(serde_json::Value::as_bool).unwrap_or(false),
            velocity_mps: number(9),
            true_track: number(10),
            vertical_rate_mps: number(11),
            geo_altitude_m: number(13),
            squawk: text(14),
            // OpenSky's categories are 0-20; anything outside a byte is not a
            // category this plugin has a mapping for, so it reads as absent.
            category: at(17)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u8::try_from(value).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A receiver's own `aircraft.json`, written by hand: an airliner, a
    /// helicopter with no geometric altitude, a military airframe, a vehicle on
    /// the ground, a TIS-B target with a `~` address, one whose position has
    /// gone stale and one that never had a position at all.
    const READSB: &str = include_str!("../tests/fixtures/readsb.json");

    #[test]
    fn a_receiver_snapshot_reads_the_fields_the_mapping_needs() {
        let snapshot: Snapshot = serde_json::from_str(READSB).expect("the fixture parses");

        assert_eq!(snapshot.now, Some(1_789_000_000.0));

        let aircraft = snapshot.into_aircraft();

        assert_eq!(aircraft.len(), 7);
        assert_eq!(aircraft[0].hex.as_deref(), Some("3c6444"));
        assert_eq!(aircraft[0].flight.as_deref(), Some("BAW117  "));
        assert_eq!(aircraft[0].alt_geom, Some(3_225.0));
        assert_eq!(aircraft[0].category.as_deref(), Some("A5"));
        assert!(!aircraft[0].military());
    }

    #[test]
    fn the_ground_sentinel_is_a_position_rather_than_an_error() {
        let aircraft: Aircraft =
            serde_json::from_str(r#"{"hex":"4ca7b3","alt_baro":"ground","lat":51.47,"lon":-0.45}"#)
                .expect("a ground target parses");

        let altitude = aircraft.alt_baro.expect("the sentinel is carried");

        assert!(altitude.on_ground());
        assert_eq!(altitude.feet(), None);
    }

    #[test]
    fn a_numeric_barometric_altitude_is_still_a_number() {
        let aircraft: Aircraft =
            serde_json::from_str(r#"{"hex":"3c6444","alt_baro":2675}"#).expect("it parses");

        let altitude = aircraft.alt_baro.expect("the altitude is carried");

        assert_eq!(altitude.feet(), Some(2_675.0));
        assert!(!altitude.on_ground());
    }

    #[test]
    fn the_military_flag_is_the_low_bit_of_db_flags() {
        let military: Aircraft =
            serde_json::from_str(r#"{"hex":"43c1d2","dbFlags":1}"#).expect("it parses");
        let interesting: Aircraft =
            serde_json::from_str(r#"{"hex":"400f1a","dbFlags":2}"#).expect("it parses");

        assert!(military.military());
        assert!(
            !interesting.military(),
            "bit 2 is 'interesting', not military"
        );
    }

    #[test]
    fn both_aggregator_array_keys_answer_the_same_aircraft() {
        // adsb.lol and airplanes.live say `ac`; readsb and adsb.fi say
        // `aircraft`. One parser, two keys, because the objects inside are the
        // same objects.
        let lol: Snapshot =
            serde_json::from_str(r#"{"now":1.0,"total":1,"ac":[{"hex":"3c6444"}]}"#).unwrap();
        let fi: Snapshot =
            serde_json::from_str(r#"{"now":1.0,"resultCount":1,"aircraft":[{"hex":"3c6444"}]}"#)
                .unwrap();

        assert_eq!(lol.into_aircraft()[0].hex.as_deref(), Some("3c6444"));
        assert_eq!(fi.into_aircraft()[0].hex.as_deref(), Some("3c6444"));
    }

    #[test]
    fn a_document_with_neither_key_is_an_empty_sky() {
        let empty: Snapshot = serde_json::from_str(r#"{"now":1.0,"messages":0}"#).unwrap();

        assert!(empty.into_aircraft().is_empty());
    }

    #[test]
    fn an_unknown_field_does_not_stop_us_reading_the_known_ones() {
        // A readsb release that adds a field must not take this plugin down.
        let aircraft: Aircraft =
            serde_json::from_str(r#"{"hex":"3c6444","something_new":{"a":1}}"#).expect("it parses");

        assert_eq!(aircraft.hex.as_deref(), Some("3c6444"));
    }

    #[test]
    fn an_opensky_state_vector_reads_by_position_and_tolerates_nulls() {
        let states: OpenSkyStates = serde_json::from_str(
            r#"{"time":1789000000,"states":[
                ["3c6444","BAW117  ","United Kingdom",1789000000,1789000000,-0.3947,51.4602,
                 762.0,false,77.2,88.0,2.5,null,801.6,"5271",false,0,5]
            ]}"#,
        )
        .expect("the response parses");

        let state = &states.states.expect("states are present")[0];

        assert_eq!(state.icao24.as_deref(), Some("3c6444"));
        assert_eq!(state.callsign.as_deref(), Some("BAW117  "));
        assert_eq!(state.latitude, Some(51.4602));
        assert_eq!(state.longitude, Some(-0.3947));
        assert_eq!(state.baro_altitude_m, Some(762.0));
        assert_eq!(state.geo_altitude_m, Some(801.6));
        assert_eq!(state.velocity_mps, Some(77.2));
        assert_eq!(state.squawk.as_deref(), Some("5271"));
        assert_eq!(state.category, Some(5));
        assert!(!state.on_ground);
    }

    #[test]
    fn an_opensky_row_that_is_all_nulls_is_a_vector_with_nothing_in_it() {
        let states: OpenSkyStates = serde_json::from_str(
            r#"{"time":1,"states":[["4008f2",null,null,null,1,null,null,null,null,null,null,null,null,null,null,null,0]]}"#,
        )
        .expect("the response parses");

        let state = &states.states.expect("states are present")[0];

        assert_eq!(state.icao24.as_deref(), Some("4008f2"));
        assert_eq!(state.callsign, None);
        assert_eq!(state.latitude, None);
        assert_eq!(state.category, None, "no extended=1, so index 17 is absent");
        assert!(!state.on_ground);
    }

    #[test]
    fn an_empty_box_answers_null_states_rather_than_an_empty_array() {
        let states: OpenSkyStates =
            serde_json::from_str(r#"{"time":1789000000,"states":null}"#).expect("it parses");

        assert!(states.states.is_none());
    }
}
