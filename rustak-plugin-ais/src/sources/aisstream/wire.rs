//! The JSON AISStream.io sends, and what this plugin keeps of it.
//!
//! Every message is the same envelope — a `MessageType`, a `MetaData` block
//! that always carries the MMSI and the fix, and a `Message` object with one
//! key, named after the type — so one [`Envelope`] covers all four of the
//! message types this plugin subscribes to.
//!
//! # Nothing here refuses an unknown field
//!
//! The rest of rustak puts `deny_unknown_fields` on everything, because an
//! unrecognised key in a *configuration file* is an operator's typo. This is
//! the opposite case: the shape belongs to somebody else, who may add a field
//! any Tuesday, and a feed that stopped decoding positions because a new one
//! appeared would be a worse feed. Every field is therefore optional and
//! anything unrecognised is dropped.

use chrono::{DateTime, Utc};
use rustak_client::feed::Area;
use serde::Deserialize;

use crate::mapping::{Dimensions, Position, StaticData};
use crate::vessels::Observation;

/// The message types worth subscribing to: the ones that carry a position or a
/// name. Everything else AIS carries — safety broadcasts, base-station reports,
/// aids to navigation — is not a vessel on a map.
pub const MESSAGE_TYPES: [&str; 4] = [
    "PositionReport",
    "ShipStaticData",
    "StandardClassBPositionReport",
    "ExtendedClassBPositionReport",
];

/// One message from the stream.
#[derive(Debug, Deserialize)]
pub struct Envelope {
    /// Which of the message types this is; also the key inside `Message`.
    #[serde(rename = "MessageType")]
    pub message_type: String,

    /// The fields the stream adds to every message, whatever its type.
    #[serde(rename = "MetaData")]
    pub metadata: MetaData,

    /// The decoded message, under a single key named by `MessageType`.
    #[serde(rename = "Message", default)]
    pub message: serde_json::Value,
}

/// What the stream knows about a message regardless of its type.
#[derive(Debug, Deserialize)]
pub struct MetaData {
    /// The vessel's MMSI.
    #[serde(rename = "MMSI")]
    pub mmsi: u32,

    /// The vessel's name, when the stream has heard a static report for it.
    #[serde(rename = "ShipName", default)]
    pub ship_name: Option<String>,

    /// The fix, repeated out of the message body. Lower case here, unlike
    /// everywhere else in this document.
    #[serde(default)]
    pub latitude: Option<f64>,

    /// The fix's longitude.
    #[serde(default)]
    pub longitude: Option<f64>,

    /// When the message was received, in Go's default instant format.
    #[serde(default)]
    pub time_utc: Option<String>,
}

/// A class A or class B position report.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PositionBody {
    /// Course over ground, degrees true. 360 means "not available".
    pub cog: Option<f64>,
    /// Speed over ground, knots. 102.3 means "not available".
    pub sog: Option<f64>,
    /// True heading, degrees. 511 means "not available".
    pub true_heading: Option<f64>,
    /// The navigational status code. Class B reports carry none.
    pub navigational_status: Option<u8>,
    /// Latitude, decimal degrees.
    pub latitude: Option<f64>,
    /// Longitude, decimal degrees.
    pub longitude: Option<f64>,
    /// Whether the decoder believes the report. Absent means "did not say".
    pub valid: Option<bool>,
}

/// A type 5 or type 24 static report.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct StaticBody {
    /// The vessel's name.
    pub name: Option<String>,
    /// Its radio call sign.
    pub call_sign: Option<String>,
    /// Its IMO number.
    pub imo_number: Option<u32>,
    /// The combined ship-and-cargo type code.
    #[serde(rename = "Type")]
    pub ship_type: Option<u8>,
    /// Where it says it is going.
    pub destination: Option<String>,
    /// When it says it will get there.
    pub eta: Option<Eta>,
    /// The four distances that give its length and beam.
    pub dimension: Option<Dimension>,
}

/// An estimated arrival, as AIS carries it: no year, and 0 for "not set".
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Eta {
    /// 1–12, or 0 for "not set".
    pub month: Option<u8>,
    /// 1–31, or 0 for "not set".
    pub day: Option<u8>,
    /// 0–23, or 24 for "not set".
    pub hour: Option<u8>,
    /// 0–59, or 60 for "not set".
    pub minute: Option<u8>,
}

impl Eta {
    /// The ETA as a line for the remarks, or [`None`] when nobody set one.
    #[must_use]
    pub fn rendered(&self) -> Option<String> {
        let (month, day) = (self.month?, self.day?);
        let (hour, minute) = (self.hour.unwrap_or(24), self.minute.unwrap_or(60));

        if month == 0 || day == 0 || hour > 23 || minute > 59 {
            return None;
        }

        Some(format!("{month:02}-{day:02} {hour:02}:{minute:02}"))
    }
}

/// A hull's extent, in metres from its position reference point.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Dimension {
    /// To the bow.
    pub a: Option<u16>,
    /// To the stern.
    pub b: Option<u16>,
    /// To the port side.
    pub c: Option<u16>,
    /// To the starboard side.
    pub d: Option<u16>,
}

impl Envelope {
    /// The observation this message carries, or [`None`] when it carries none
    /// this plugin can use.
    #[must_use]
    pub fn observation(&self) -> Option<Observation> {
        let body = self.message.get(&self.message_type)?;

        if self.message_type == "ShipStaticData" {
            let statics: StaticBody = serde_json::from_value(body.clone()).ok()?;

            return Some(Observation::Static(self.statics(&statics)));
        }

        let position: PositionBody = serde_json::from_value(body.clone()).ok()?;

        // `Valid` is the decoder's own verdict; absent means it did not offer
        // one, which is not the same as saying no.
        if position.valid == Some(false) {
            return None;
        }

        Some(Observation::Position(self.position(&position)))
    }

    /// The position this message describes, preferring the message body's own
    /// fix over the metadata's copy of it.
    fn position(&self, body: &PositionBody) -> Position {
        Position {
            mmsi: self.metadata.mmsi,
            position: (
                body.latitude.or(self.metadata.latitude).unwrap_or(91.0),
                body.longitude.or(self.metadata.longitude).unwrap_or(181.0),
            ),
            sog_knots: body.sog,
            cog_deg: body.cog,
            heading_deg: body.true_heading,
            nav_status: body.navigational_status,
            observed_at: self.observed_at(),
        }
    }

    /// The static data this message describes.
    fn statics(&self, body: &StaticBody) -> StaticData {
        let dimensions = body.dimension.as_ref().map(|dimension| Dimensions {
            to_bow: dimension.a.unwrap_or_default(),
            to_stern: dimension.b.unwrap_or_default(),
            to_port: dimension.c.unwrap_or_default(),
            to_starboard: dimension.d.unwrap_or_default(),
        });

        StaticData {
            mmsi: self.metadata.mmsi,
            name: trimmed(body.name.as_deref().or(self.metadata.ship_name.as_deref())),
            call_sign: trimmed(body.call_sign.as_deref()),
            imo: body.imo_number,
            ship_type: body.ship_type,
            destination: trimmed(body.destination.as_deref()),
            eta: body.eta.as_ref().and_then(Eta::rendered),
            dimensions,
        }
    }

    /// When this message was received, or now for one that did not say.
    fn observed_at(&self) -> DateTime<Utc> {
        self.metadata
            .time_utc
            .as_deref()
            .and_then(instant)
            .unwrap_or_else(Utc::now)
    }
}

/// Parses the instant AISStream writes, which is Go's default format —
/// `2026-09-20 12:00:00.123456789 +0000 UTC` — with RFC 3339 accepted too, so
/// that a change of format upstream is a value this still reads.
#[must_use]
pub fn instant(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim().trim_end_matches("UTC").trim();

    DateTime::parse_from_rfc3339(trimmed)
        .or_else(|_| DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f %z"))
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// AIS pads its text fields with `@` and spaces; neither is part of a name.
fn trimmed(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim_matches(|character: char| character == '@' || character.is_whitespace());

    match value.is_empty() {
        true => None,
        false => Some(value.to_string()),
    }
}

/// The subscription message, which must be sent within three seconds of the
/// socket opening or the server closes it.
///
/// Latitude first in every pair, and an area that crosses the anti-meridian
/// becomes two boxes, because a box whose west is east of its east is a box
/// covering the rest of the world.
#[must_use]
pub fn subscription(api_key: &str, area: Area, message_types: &[String]) -> String {
    let Area::Bbox {
        south,
        west,
        north,
        east,
    } = area.bbox()
    else {
        unreachable!("Area::bbox always answers a box");
    };

    let boxes = match west <= east {
        true => vec![[[south, west], [north, east]]],
        false => vec![
            [[south, west], [north, 180.0]],
            [[south, -180.0], [north, east]],
        ],
    };

    serde_json::json!({
        "APIKey": api_key,
        "BoundingBoxes": boxes,
        "FilterMessageTypes": message_types,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written by hand from the documented shape, as every fixture in this
    /// repository is: no captured traffic is copied into rustak.
    fn decode(json: &str) -> Envelope {
        serde_json::from_str(json).expect("the envelope decodes")
    }

    #[test]
    fn a_class_a_position_report_becomes_a_position() {
        let envelope = decode(
            r#"{
                "MessageType": "PositionReport",
                "MetaData": {"MMSI": 244660000, "ShipName": "ZEEBRUGGE  ",
                             "latitude": 51.9512, "longitude": 4.1338,
                             "time_utc": "2026-09-20 12:00:00.123456789 +0000 UTC"},
                "Message": {"PositionReport": {"Cog": 271.5, "Sog": 6.2, "TrueHeading": 270,
                                               "NavigationalStatus": 0, "Latitude": 51.9513,
                                               "Longitude": 4.1339, "Valid": true,
                                               "UserID": 244660000}}
            }"#,
        );

        let Some(Observation::Position(position)) = envelope.observation() else {
            panic!("a position report is a position");
        };

        assert_eq!(position.mmsi, 244_660_000);
        assert_eq!(
            position.position,
            (51.9513, 4.1339),
            "the body's fix, not the metadata's copy",
        );
        assert_eq!(position.sog_knots, Some(6.2));
        assert_eq!(position.cog_deg, Some(271.5));
        assert_eq!(position.heading_deg, Some(270.0));
        assert_eq!(position.nav_status, Some(0));
        assert_eq!(
            position.observed_at.to_rfc3339(),
            "2026-09-20T12:00:00.123456789+00:00",
        );
    }

    #[test]
    fn a_ship_static_data_message_becomes_static_data() {
        let envelope = decode(
            r#"{
                "MessageType": "ShipStaticData",
                "MetaData": {"MMSI": 244660000, "ShipName": "ZEEBRUGGE",
                             "latitude": 51.95, "longitude": 4.13,
                             "time_utc": "2026-09-20 12:00:05 +0000 UTC"},
                "Message": {"ShipStaticData": {"Name": "ZEEBRUGGE@@@@@", "CallSign": "PBZE  ",
                                               "ImoNumber": 9312345, "Type": 70,
                                               "Destination": "ROTTERDAM",
                                               "Eta": {"Month": 9, "Day": 21, "Hour": 6, "Minute": 0},
                                               "Dimension": {"A": 120, "B": 30, "C": 11, "D": 11}}}
            }"#,
        );

        let Some(Observation::Static(statics)) = envelope.observation() else {
            panic!("a static report is static data");
        };

        assert_eq!(
            statics.name.as_deref(),
            Some("ZEEBRUGGE"),
            "padding trimmed"
        );
        assert_eq!(statics.call_sign.as_deref(), Some("PBZE"));
        assert_eq!(statics.imo, Some(9_312_345));
        assert_eq!(statics.ship_type, Some(70));
        assert_eq!(statics.destination.as_deref(), Some("ROTTERDAM"));
        assert_eq!(statics.eta.as_deref(), Some("09-21 06:00"));
        assert_eq!(statics.dimensions.expect("dimensions").length_m(), 150);
    }

    #[test]
    fn both_class_b_position_reports_decode_without_a_navigational_status() {
        for message_type in [
            "StandardClassBPositionReport",
            "ExtendedClassBPositionReport",
        ] {
            let envelope = decode(&format!(
                r#"{{
                    "MessageType": "{message_type}",
                    "MetaData": {{"MMSI": 244123456, "latitude": 51.8977, "longitude": 4.0104,
                                  "time_utc": "2026-09-20 12:00:00 +0000 UTC"}},
                    "Message": {{"{message_type}": {{"Sog": 4.1, "Cog": 88.0,
                                                     "TrueHeading": 511, "Latitude": 51.8977,
                                                     "Longitude": 4.0104, "Valid": true}}}}
                }}"#,
            ));

            let Some(Observation::Position(position)) = envelope.observation() else {
                panic!("{message_type} is a position");
            };

            assert_eq!(position.nav_status, None, "{message_type}");
            assert_eq!(position.heading_deg, Some(511.0), "the sentinel survives");
            assert_eq!(position.sog_knots, Some(4.1), "{message_type}");
        }
    }

    #[test]
    fn a_message_the_decoder_flagged_invalid_is_dropped() {
        let envelope = decode(
            r#"{
                "MessageType": "PositionReport",
                "MetaData": {"MMSI": 1, "latitude": 0, "longitude": 0, "time_utc": ""},
                "Message": {"PositionReport": {"Latitude": 0, "Longitude": 0, "Valid": false}}
            }"#,
        );

        assert!(envelope.observation().is_none());
    }

    #[test]
    fn a_message_whose_body_is_missing_or_unknown_is_dropped_not_fatal() {
        let subscribed = decode(
            r#"{"MessageType": "SubscriptionError",
                "MetaData": {"MMSI": 0, "time_utc": ""}, "Message": {}}"#,
        );

        assert!(subscribed.observation().is_none());
    }

    #[test]
    fn a_field_nobody_told_us_about_is_ignored_rather_than_fatal() {
        // The opposite of every configuration type in rustak: this shape is
        // somebody else's, and a new key must not stop the feed.
        let envelope = decode(
            r#"{
                "MessageType": "PositionReport",
                "MetaData": {"MMSI": 7, "latitude": 51.9, "longitude": 4.1,
                             "time_utc": "2026-09-20 12:00:00 +0000 UTC", "Whatever": 3},
                "Message": {"PositionReport": {"Latitude": 51.9, "Longitude": 4.1,
                                               "SomethingNew": {"nested": true}}}
            }"#,
        );

        assert!(matches!(
            envelope.observation(),
            Some(Observation::Position(_)),
        ));
    }

    #[test]
    fn an_unset_eta_is_no_eta_at_all() {
        assert_eq!(Eta::default().rendered(), None);
        assert_eq!(
            Eta {
                month: Some(0),
                day: Some(0),
                hour: Some(24),
                minute: Some(60),
            }
            .rendered(),
            None,
            "AIS spells 'not set' as out-of-range values",
        );
        assert_eq!(
            Eta {
                month: Some(1),
                day: Some(2),
                hour: Some(3),
                minute: Some(4),
            }
            .rendered()
            .as_deref(),
            Some("01-02 03:04"),
        );
    }

    #[test]
    fn a_subscription_names_the_box_latitude_first() {
        let body = subscription(
            "k3y",
            Area::Bbox {
                south: 51.7,
                west: 3.6,
                north: 52.3,
                east: 4.7,
            },
            &["PositionReport".to_string()],
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        assert_eq!(parsed["APIKey"], "k3y");
        assert_eq!(parsed["BoundingBoxes"][0][0][0], 51.7, "latitude first");
        assert_eq!(parsed["BoundingBoxes"][0][0][1], 3.6);
        assert_eq!(parsed["BoundingBoxes"][0][1][0], 52.3);
        assert_eq!(parsed["BoundingBoxes"][0][1][1], 4.7);
        assert_eq!(parsed["FilterMessageTypes"][0], "PositionReport");
    }

    #[test]
    fn a_circle_subscribes_as_its_enclosing_box() {
        let body = subscription(
            "k3y",
            Area::Circle {
                lat: 51.95,
                lon: 4.13,
                radius_km: 60.0,
            },
            &[],
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        assert_eq!(
            parsed["BoundingBoxes"].as_array().expect("one box").len(),
            1
        );
        assert!(parsed["BoundingBoxes"][0][0][0].as_f64().expect("south") < 51.95);
        assert!(parsed["BoundingBoxes"][0][1][0].as_f64().expect("north") > 51.95);
    }

    #[test]
    fn an_area_across_the_anti_meridian_subscribes_as_two_boxes() {
        let body = subscription(
            "k3y",
            Area::Bbox {
                south: -45.0,
                west: 170.0,
                north: -35.0,
                east: -170.0,
            },
            &[],
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let boxes = parsed["BoundingBoxes"].as_array().expect("two boxes");

        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0][1][1], 180.0);
        assert_eq!(boxes[1][0][1], -180.0);
    }

    #[test]
    fn the_instant_format_is_read_either_way_round() {
        assert_eq!(
            instant("2026-09-20 12:00:00 +0000 UTC")
                .expect("Go's format")
                .to_rfc3339(),
            "2026-09-20T12:00:00+00:00",
        );
        assert_eq!(
            instant("2026-09-20T12:00:00Z")
                .expect("RFC 3339")
                .to_rfc3339(),
            "2026-09-20T12:00:00+00:00",
        );
        assert_eq!(instant("whenever"), None);
    }
}
