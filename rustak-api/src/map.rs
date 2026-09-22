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

    /// The channels the sender was publishing into, resolved to names.
    #[serde(default)]
    pub groups: Vec<String>,
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
            course: Some(90.0),
            speed: Some(1.5),
            battery: None,
            remarks: None,
            software: None,
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
}
