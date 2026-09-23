//! The map: what is drawn, and what changes while somebody is looking at it.
//!
//! Two reads answer the two halves of "show me the situation".
//! `GET /api/v1/map/features` is everything worth drawing right now, as a list
//! of [`MapFeature`]; `GET /api/v1/map/events` is a Server-Sent Events response
//! carrying a [`MapUpdate`] for each thing that changes afterwards. A page
//! opens the feed first, reads the snapshot second and merges by
//! [`time`](MapFeature::time), so nothing relayed in between is lost and
//! nothing older overwrites something newer.
//!
//! # Writing is one more shape
//!
//! `PUT /api/v1/map/features/{uid}` takes a [`PublishFeature`] — what a page
//! can say about a marker it placed or a drawing it made — and the server makes a CoT
//! event of it, injected as though a client had sent it: tagged, recorded,
//! fanned out to devices, and fed back to every open map. `DELETE` on the
//! same path sends the delete every TAK client understands. The answer to
//! both is the [`MapFeature`] the map now draws, or no longer does.
//!
//! # A feature is a CoT event, flattened
//!
//! Everything a marker and its pop-over need, parsed once on the server so the
//! browser never has to read XML: where, what, who, how fast, how stale. The
//! XML itself stays behind [`CotDetail`](crate::CotDetail), one request away.
//!
//! # Shapes are GeoJSON, points are not
//!
//! Every CoT event has a `<point>`, so every feature has a [`MapPoint`] — the
//! anchor a marker and a pop-over hang from. A drawing additionally carries a
//! [`MapShape`], spelled as a GeoJSON geometry (`[lon, lat]` order and all) so
//! a map library can be handed it untouched. Keeping both is what lets an
//! editor later move a shape's anchor and its outline independently, the way
//! TAK clients do.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One thing on the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapFeature {
    /// The CoT event uid, which is the key a feature is replaced and removed by.
    pub uid: String,

    /// The CoT type, `a-f-G-U-C` and its siblings.
    #[serde(rename = "type")]
    pub kind: String,

    /// How the position was produced: `m-g` for GPS, `h-e` for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    /// The team colour from `<__group name>`, when the message carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,

    /// The role from `<__group role>`, when the message carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// The event's own time, which is what two copies of a uid are ordered by.
    pub time: DateTime<Utc>,

    /// When the event stops being current. A page dims it then, and drops it a
    /// little later.
    pub stale: DateTime<Utc>,

    /// When this server relayed it.
    pub received_at: DateTime<Utc>,

    pub point: MapPoint,

    /// The outline, for a drawing. [`None`] for everything that is only a point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<MapShape>,

    /// What the outline was drawn from, when that was a circle or an ellipse
    /// around the point: the [`shape`](Self::shape) is then the ring a map
    /// draws, and this is what an editor changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ellipse: Option<MapEllipse>,

    /// How the sender asked for the outline to be drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<MapStyle>,

    /// Degrees clockwise from true north.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course: Option<f64>,

    /// Metres per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,

    /// Percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remarks: Option<String>,

    /// What sent it, from `<takv>`: `ATAK-CIV 5.2 on a Pixel 8`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,

    /// The symbol identification code the sender asked to be drawn with, from
    /// `<__milicon id>` or `<__milsym id>`, in whichever edition of
    /// MIL-STD-2525 it was written: see [`sidc`]. [`None`] leaves the symbol to
    /// the type, which is what most events do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidc: Option<String>,

    /// The channels the sender was publishing into, resolved to names.
    #[serde(default)]
    pub groups: Vec<String>,
}

/// A symbol identification code as a map is handed one, or [`None`] for text
/// that is not one.
///
/// Fifteen characters of letters, `-` and `*` is MIL-STD-2525C (and B, and
/// APP-6); twenty digits is 2525D and thirty is 2525E. ATAK's schema for the
/// detail says only that "it is up to the processing system to process the
/// identifier correctly", so anything else is left out here rather than handed
/// to a page to guess at.
#[must_use]
pub fn sidc(text: &str) -> Option<String> {
    let text = text.trim();

    let letters = text.len() == 15
        && text
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '*');
    let digits = matches!(text.len(), 20 | 30) && text.chars().all(|c| c.is_ascii_digit());

    (letters || digits).then(|| text.to_ascii_uppercase())
}

/// Where an event says it is.
///
/// CoT spells "unknown" as `9999999.0`; here it is an absent key, so a page
/// never has to know the sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MapPoint {
    pub lat: f64,
    pub lon: f64,

    /// Metres above the WGS-84 ellipsoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hae: Option<f64>,

    /// Circular error, in metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ce: Option<f64>,

    /// Linear (vertical) error, in metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub le: Option<f64>,
}

/// A drawing's outline, as a GeoJSON geometry.
///
/// Positions are `[lon, lat]`, which is GeoJSON's order and the opposite of
/// CoT's — the one place in this API where the two meet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "coordinates")]
pub enum MapShape {
    LineString(Vec<[f64; 2]>),

    /// Rings, the first of which is the outline. Each is closed: its last
    /// position repeats its first.
    Polygon(Vec<Vec<[f64; 2]>>),
}

/// A circle or an ellipse around a feature's point, as TAK clients write one
/// in `<shape><ellipse>`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MapEllipse {
    /// The semi-major axis, in metres: a circle's radius.
    pub major: f64,

    /// The semi-minor axis, in metres: a circle's radius again.
    pub minor: f64,

    /// The major axis's bearing, in degrees clockwise from north.
    #[serde(default)]
    pub angle: f64,
}

/// How a drawing is drawn, from the elements TAK clients write beside an
/// outline: `<strokeColor>`, `<strokeWeight>` and `<fillColor>`.
///
/// Colours are `#rrggbb`. CoT spells them as signed 32-bit ARGB integers,
/// which is nothing a page should have to know; the alpha of a fill is its
/// [`fill_opacity`](Self::fill_opacity), and the alpha of a line is not kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,

    /// The line's width as TAK clients count it, where 4 is what ATAK draws a
    /// new shape with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,

    /// From 0, which is no fill, to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_opacity: Option<f64>,
}

/// `#rrggbb` as its red, green and blue, or [`None`] for text that is not a
/// colour written that way.
#[must_use]
pub fn rgb(color: &str) -> Option<[u8; 3]> {
    let digits = color.strip_prefix('#').filter(|digits| digits.len() == 6)?;
    let value = u32::from_str_radix(digits, 16).ok()?;

    Some([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}

/// What a page publishes: a marker or a drawing, as somebody made or edited it.
///
/// Everything a CoT event needs that the page can sensibly decide. The uid is
/// the page's — minted when a marker is placed and kept when it is edited, so
/// that a `PUT` replaces rather than duplicates. What is left out — the time,
/// the flow tag, the sender — is the server's to add.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishFeature {
    /// The CoT type: `a-u-G` for an unknown thing on the ground, `b-m-p-s-m`
    /// for a spot marker.
    #[serde(rename = "type")]
    pub kind: String,

    pub callsign: String,

    pub point: MapPoint,

    /// How the position was produced. `h-g-i-g-o` — placed on a map by a
    /// person — when the page does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remarks: Option<String>,

    /// The symbol to draw it with, as [`sidc`] accepts. Left to the type when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidc: Option<String>,

    /// When it stops being current. A day from now when the page does not
    /// say, which is what a marker somebody placed by hand deserves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<DateTime<Utc>>,

    /// The channels to publish into, by name. Empty is a broadcast to
    /// everybody the sender reaches.
    #[serde(default)]
    pub groups: Vec<String>,

    /// The outline of a drawing, and only of one. The type says what it is an
    /// outline *of*, in the types ATAK's own drawing tools write: a
    /// [`LineString`](MapShape::LineString) is a line under `u-d-f` and a
    /// route under `b-m-r`; a [`Polygon`](MapShape::Polygon) of one ring is a
    /// polygon under `u-d-f` and, with four corners, a rectangle under
    /// `u-d-r`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<MapShape>,

    /// A circle around [`point`](Self::point), under `u-d-c-c`, in place of a
    /// [`shape`](Self::shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ellipse: Option<MapEllipse>,

    /// How a drawing is drawn. Left to whoever draws it when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<MapStyle>,
}

/// One change to the map, as the feed reports it.
///
/// `#[non_exhaustive]` for the reason [`ServerEventPayload`] is: a page skips a
/// frame it has never heard of rather than ending the stream.
///
/// [`ServerEventPayload`]: crate::event::ServerEventPayload
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
#[non_exhaustive]
pub enum MapUpdate {
    /// A feature arrived, or replaced what its uid said before.
    Upsert(Box<MapFeature>),

    /// Its owner deleted it.
    Remove { uid: String },

    /// The feed fell too far behind to say what changed. Read the snapshot
    /// again.
    Reset,
}

impl MapUpdate {
    /// The update's name, which is also the SSE `event:` field.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Upsert(_) => "upsert",
            Self::Remove { .. } => "remove",
            Self::Reset => "reset",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature() -> MapFeature {
        MapFeature {
            uid: "ANDROID-1".to_string(),
            kind: "a-f-G-U-C".to_string(),
            how: Some("m-g".to_string()),
            callsign: Some("ALPHA".to_string()),
            team: None,
            role: None,
            time: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            stale: "2026-09-18T12:02:00.500Z".parse().unwrap(),
            received_at: "2026-09-18T12:00:00.600Z".parse().unwrap(),
            point: MapPoint {
                lat: 51.5,
                lon: -0.12,
                hae: Some(35.0),
                ce: None,
                le: None,
            },
            shape: None,
            ellipse: None,
            style: None,
            course: Some(90.0),
            speed: Some(1.5),
            battery: None,
            remarks: None,
            software: None,
            sidc: None,
            groups: vec!["Blue".to_string()],
        }
    }

    #[test]
    fn every_update_round_trips_under_the_name_it_is_published_as() {
        for update in [
            MapUpdate::Upsert(Box::new(feature())),
            MapUpdate::Remove {
                uid: "ANDROID-1".to_string(),
            },
            MapUpdate::Reset,
        ] {
            let json = serde_json::to_value(&update).unwrap();

            assert_eq!(json["op"], update.name());
            assert_eq!(serde_json::from_value::<MapUpdate>(json).unwrap(), update);
        }
    }

    #[test]
    fn an_upsert_reads_as_the_feature_itself() {
        // Internally tagged, so a page reads `uid` and `point` off the frame
        // rather than off something nested inside it.
        let json = serde_json::to_value(MapUpdate::Upsert(Box::new(feature()))).unwrap();

        assert_eq!(json["uid"], "ANDROID-1");
        assert_eq!(json["type"], "a-f-G-U-C");
        assert_eq!(json["point"]["lat"], 51.5);
    }

    #[test]
    fn what_a_message_did_not_say_is_an_absent_key() {
        let json = serde_json::to_value(feature()).unwrap();

        assert!(json.get("team").is_none());
        assert!(json.get("shape").is_none());
        assert!(json["point"].get("ce").is_none());
    }

    #[test]
    fn a_shape_is_a_geojson_geometry() {
        let json = serde_json::to_value(MapShape::Polygon(vec![vec![
            [-0.12, 51.5],
            [-0.11, 51.5],
            [-0.11, 51.51],
            [-0.12, 51.5],
        ]]))
        .unwrap();

        assert_eq!(json["type"], "Polygon");
        assert_eq!(json["coordinates"][0][1], serde_json::json!([-0.11, 51.5]));
    }

    #[test]
    fn what_a_page_leaves_unsaid_when_publishing_is_an_absent_key() {
        let json = serde_json::to_value(PublishFeature {
            kind: "a-u-G".to_string(),
            callsign: "MARKER 1".to_string(),
            point: MapPoint {
                lat: 51.5,
                lon: -0.12,
                hae: None,
                ce: None,
                le: None,
            },
            how: None,
            remarks: None,
            sidc: None,
            stale: None,
            groups: Vec::new(),
            shape: None,
            ellipse: None,
            style: None,
        })
        .unwrap();

        assert_eq!(json["type"], "a-u-G");
        assert!(json.get("how").is_none());
        assert!(json.get("stale").is_none());
        assert_eq!(json["groups"], serde_json::json!([]));

        let read: PublishFeature =
            serde_json::from_str(r#"{"type":"a-u-G","callsign":"M","point":{"lat":1,"lon":2}}"#)
                .unwrap();
        assert_eq!(read.point.lon, 2.0);
        assert!(read.groups.is_empty());
    }

    #[test]
    fn only_text_shaped_like_a_symbol_code_is_one() {
        for (text, expected) in [
            ("SFGPUCI--------", Some("SFGPUCI--------")),
            (" sfgpuci----*---\n", Some("SFGPUCI----*---")),
            ("10031000001211000000", Some("10031000001211000000")),
            (
                "130310000012110000000000000000",
                Some("130310000012110000000000000000"),
            ),
            ("", None),
            ("a-f-G-U-C", None),
            ("1003100000121100000", None),
            ("sidc:SFGPUCI-----", None),
        ] {
            assert_eq!(sidc(text).as_deref(), expected, "{text:?}");
        }
    }
}
