//! One observation of one moving thing, and the CoT event it becomes.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_cot::detail::{Contact, Remarks};
use rustak_cot::{CotTime, Event, Point};
use serde::{Deserialize, Serialize};

use super::{Affiliation, TrackKind};

/// How a feed says an object was located: `m-g` is "machine, GPS", which is
/// what AIS and ADS-B both are — a position the object itself reported.
const HOW: &str = "m-g";

/// What a feed knows about one object at one moment.
///
/// This is the whole contract between a source plugin and the publisher: a
/// source turns whatever its upstream speaks into these, and everything after
/// that — throttling, staleness, the symbol on the map — is the same for ships
/// and aircraft alike.
///
/// It is `Serialize` as well as `Deserialize` because the replay source reads a
/// file of these, one JSON object per line: a fixture written by a test is read
/// back by a plugin without a second format in between.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// The uid this object appears on the map as, **already prefixed by its
    /// source** — `AIS-244660000`, `ADSB-3c6444`.
    ///
    /// The prefix is what keeps two feeds watching the same airport from
    /// overwriting each other, and what lets an operator tell at a glance where
    /// a track came from. A source that published a bare MMSI would also be
    /// claiming every other feed's right to that uid.
    pub id: String,

    /// What this is, which decides the CoT type.
    pub kind: TrackKind,

    /// Where it is: latitude and longitude in decimal degrees, WGS-84.
    pub position: (f64, f64),

    /// Height above the WGS-84 ellipsoid, metres. [`None`] for a vessel, and
    /// for an aircraft whose report carried only a barometric altitude that the
    /// source chose not to convert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude_hae_m: Option<f64>,

    /// Speed over ground, metres per second — the unit CoT's `<track>` carries,
    /// so a source converts knots once rather than every consumer converting
    /// them back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mps: Option<f64>,

    /// Course over ground, degrees true: the direction it is *travelling*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course_deg: Option<f64>,

    /// Heading, degrees true: the direction it is *pointing*, which differs
    /// from the course in a current or a crosswind.
    ///
    /// CoT's `<track>` has nowhere to put this, so it is used as the course
    /// when no course was reported and kept for the plugin otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_deg: Option<f64>,

    /// What the map shows: a ship's name, an aircraft's flight number. [`None`]
    /// for a report that carried no name, which leaves the uid showing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    /// Ordered key/value lines rendered into `<remarks>` as `key: value`, which
    /// is what an operator sees when they tap the track.
    ///
    /// Ordered rather than a map because the order is a decision the source
    /// makes: the identifier first, then what it is, then where it is going.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remarks: Vec<(String, String)>,

    /// When the object was at this position, as its source reported it — not
    /// when we heard about it.
    pub observed_at: DateTime<Utc>,

    /// Whether it is on the ground or alongside.
    ///
    /// Carried for the source plugin's own decisions (an aircraft taxiing is a
    /// [`GroundVehicle`](TrackKind::GroundVehicle) to some operators and an
    /// aircraft to others) rather than written into the event: CoT says what a
    /// thing *is* through its type, and has no separate on-ground flag.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_ground: bool,
}

impl Track {
    /// A track with nothing but the four things every feed reports.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        kind: TrackKind,
        position: (f64, f64),
        observed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            position,
            observed_at,
            ..Self::default()
        }
    }

    /// Sets the callsign the map shows.
    #[must_use]
    pub fn with_callsign(mut self, callsign: impl Into<String>) -> Self {
        self.callsign = Some(callsign.into());
        self
    }

    /// Sets the altitude above the ellipsoid, in metres.
    #[must_use]
    pub fn with_altitude_hae_m(mut self, altitude: f64) -> Self {
        self.altitude_hae_m = Some(altitude);
        self
    }

    /// Sets speed (metres per second) and course (degrees true) together, which
    /// is how both feeds report them.
    #[must_use]
    pub fn with_velocity(mut self, speed_mps: f64, course_deg: f64) -> Self {
        self.speed_mps = Some(speed_mps);
        self.course_deg = Some(course_deg);
        self
    }

    /// Sets the heading, in degrees true.
    #[must_use]
    pub fn with_heading_deg(mut self, heading: f64) -> Self {
        self.heading_deg = Some(heading);
        self
    }

    /// Sets whether the object is on the ground or alongside.
    #[must_use]
    pub fn with_on_ground(mut self, on_ground: bool) -> Self {
        self.on_ground = on_ground;
        self
    }

    /// Appends one `key: value` line to the remarks.
    #[must_use]
    pub fn with_remark(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.remarks.push((key.into(), value.into()));
        self
    }

    /// The direction to put in `<track>`: the course when there is one, and the
    /// heading when there is not.
    #[must_use]
    pub fn bearing_deg(&self) -> Option<f64> {
        self.course_deg.or(self.heading_deg)
    }

    /// The CoT event this observation publishes as.
    ///
    /// `stale` is how long the track stays on a map without another report; the
    /// publisher takes it from its [`PublishPolicy`](super::PublishPolicy), and
    /// TAK clients expire the track by themselves when it passes, which is why
    /// a feed never sends a delete.
    ///
    /// ```
    /// use std::time::Duration;
    /// use rustak_client::feed::{Affiliation, Track, TrackKind, VesselClass};
    ///
    /// let track = Track::new(
    ///     "AIS-244660000",
    ///     TrackKind::Vessel(VesselClass::Merchant),
    ///     (51.9, 4.1),
    ///     "2026-09-20T12:00:00Z".parse().unwrap(),
    /// )
    /// .with_callsign("ZEEBRUGGE");
    ///
    /// let event = track.to_event(Affiliation::Unknown, Duration::from_secs(120));
    ///
    /// assert_eq!(event.uid, "AIS-244660000");
    /// assert_eq!(event.r#type, "a-u-S-X-M");
    /// assert_eq!(event.callsign(), Some("ZEEBRUGGE"));
    /// ```
    #[must_use]
    pub fn to_event(&self, affiliation: Affiliation, stale: Duration) -> Event {
        let time = CotTime::from_datetime(self.observed_at);
        let mut builder = Event::builder(self.kind.cot_type(affiliation), self.id.clone())
            .how(HOW)
            .point_full(Point {
                // `Point::new`'s sentinels, with the altitude filled in when the
                // feed knew one: CoT has no "unknown", so 9999999.0 is it.
                hae: self.altitude_hae_m.unwrap_or(Point::UNKNOWN_HAE),
                ..Point::new(self.position.0, self.position.1)
            })
            .time(time)
            .start(time)
            .stale(time.stale_after(stale));

        if let Some(callsign) = &self.callsign {
            // No endpoint: a feed track is a thing on the map, not a chat peer,
            // and an endpoint would invite a client to try to reach it.
            builder = builder.typed(&Contact::new(callsign.clone()));
        }

        if self.speed_mps.is_some() || self.bearing_deg().is_some() {
            builder = builder.typed(&rustak_cot::detail::Track::new(
                self.speed_mps.unwrap_or_default(),
                self.bearing_deg().unwrap_or_default(),
            ));
        }

        if !self.remarks.is_empty() {
            builder = builder.typed(&Remarks {
                text: self.remarks_text(),
                ..Remarks::default()
            });
        }

        builder.build()
    }

    /// The remarks as the lines an operator reads.
    fn remarks_text(&self) -> String {
        self.remarks
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::VesselClass;

    /// A vessel report with everything an AIS position message carries.
    fn vessel() -> Track {
        Track::new(
            "AIS-244660000",
            TrackKind::Vessel(VesselClass::Merchant),
            (51.9, 4.1),
            "2026-09-20T12:00:00Z".parse().expect("an instant"),
        )
        .with_callsign("ZEEBRUGGE")
        .with_velocity(6.2, 271.5)
        .with_remark("MMSI", "244660000")
        .with_remark("Destination", "ROTTERDAM")
    }

    #[test]
    fn a_track_publishes_as_the_xml_a_tak_client_reads() {
        // A golden on the bytes rather than on the model: what a client parses
        // is the document, and an attribute that moved or a detail that stopped
        // being emitted is invisible to an assertion on `Event`'s fields.
        let bytes = rustak_cot::xml::write(
            &vessel().to_event(Affiliation::Unknown, Duration::from_secs(120)),
        );

        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<event version=\"2.0\" uid=\"AIS-244660000\" type=\"a-u-S-X-M\" how=\"m-g\"",
                " time=\"2026-09-20T12:00:00.000Z\" start=\"2026-09-20T12:00:00.000Z\"",
                " stale=\"2026-09-20T12:02:00.000Z\">",
                "<point lat=\"51.9\" lon=\"4.1\" hae=\"9999999.0\" ce=\"9999999.0\" le=\"9999999.0\"/>",
                "<detail>",
                "<contact callsign=\"ZEEBRUGGE\"/>",
                "<track speed=\"6.2\" course=\"271.5\"/>",
                "<remarks>MMSI: 244660000\nDestination: ROTTERDAM</remarks>",
                "</detail></event>",
            )
        );
    }

    #[test]
    fn a_bare_position_report_carries_no_detail_it_has_nothing_to_say_with() {
        let track = Track::new(
            "ADSB-3c6444",
            TrackKind::Aircraft(crate::feed::AircraftClass::Unknown),
            (51.4, -0.45),
            "2026-09-20T12:00:00Z".parse().unwrap(),
        );

        let event = track.to_event(Affiliation::Unknown, Duration::from_secs(90));

        assert_eq!(event.r#type, "a-u-A");
        assert!(event.detail.is_empty(), "nothing invented out of nothing");
        assert_eq!(event.point.hae, Point::UNKNOWN_HAE);
        assert_eq!(event.point.ce, Point::UNKNOWN_CE);
        assert_eq!(event.time, event.start);
        assert_eq!(event.stale.millis() - event.time.millis(), 90_000);
    }

    #[test]
    fn an_altitude_becomes_the_height_above_the_ellipsoid() {
        let track = Track::new(
            "ADSB-3c6444",
            TrackKind::Aircraft(crate::feed::AircraftClass::CivilFixedWing),
            (51.4, -0.45),
            "2026-09-20T12:00:00Z".parse().unwrap(),
        )
        .with_altitude_hae_m(11_277.6);

        assert_eq!(
            track
                .to_event(Affiliation::Friend, Duration::from_secs(90))
                .point
                .hae,
            11_277.6
        );
    }

    #[test]
    fn a_heading_stands_in_for_a_course_nobody_reported() {
        // A vessel at anchor reports a heading and no course; a track with
        // neither publishes no `<track>` at all.
        let anchored = Track::new(
            "AIS-1",
            TrackKind::Vessel(VesselClass::Other),
            (51.9, 4.1),
            "2026-09-20T12:00:00Z".parse().unwrap(),
        )
        .with_heading_deg(45.0);

        let event = anchored.to_event(Affiliation::Unknown, Duration::from_secs(120));
        let track: rustak_cot::detail::Track = event.detail.get().expect("a track element");

        assert_eq!(track.course, 45.0);
        assert_eq!(track.speed, 0.0, "a speed nobody reported is not a speed");

        let silent = Track::new(
            "AIS-2",
            TrackKind::Vessel(VesselClass::Other),
            (51.9, 4.1),
            "2026-09-20T12:00:00Z".parse().unwrap(),
        );

        assert!(
            silent
                .to_event(Affiliation::Unknown, Duration::from_secs(120))
                .detail
                .find("track")
                .is_none()
        );
    }

    #[test]
    fn a_track_round_trips_through_the_replay_fixture_format() {
        let line = serde_json::to_string(&vessel()).expect("a fixture line");

        assert!(!line.contains('\n'), "one observation is one line");
        assert!(
            !line.contains("on_ground"),
            "a fixture writes only what it knows: {line}"
        );
        assert_eq!(
            serde_json::from_str::<Track>(&line).expect("the line reads back"),
            vessel()
        );
    }

    #[test]
    fn a_misspelled_fixture_key_is_refused_rather_than_ignored() {
        let refused = serde_json::from_str::<Track>(
            r#"{"id":"AIS-1","kind":{"vessel":"other"},"position":[51.9,4.1],
                "observed_at":"2026-09-20T12:00:00Z","speed_knots":6.2}"#,
        );

        assert!(refused.is_err());
    }
}
