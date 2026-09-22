//! What a feature looks like, decided before it reaches the map.
//!
//! Three ways to be drawn, the way TAK clients draw them. Somebody carrying a
//! device reports a team, and is a dot in that team's colour. Any other atom
//! is its MIL-STD-2525 symbol, with an arrow when it is moving. Everything
//! else — a spot marker, an alert, the anchor of a drawing — is a plain dot,
//! and a drawing brings its outline with it.
//!
//! The output is GeoJSON with a handful of properties `js/map.js` styles from.
//! Nothing here touches the browser, so all of it is tested natively.

use chrono::{DateTime, Utc};
use rustak_api::MapFeature;
use serde_json::{Value, json};

use crate::util::sidc;

/// Slower than this, in metres per second, and a course is noise from a GPS
/// that is standing still.
const MOVING: f64 = 0.5;

/// Arrows are drawn to the nearest this many degrees, so that an aircraft in a
/// turn costs the map a few images rather than three hundred and sixty.
const DIRECTION_STEP: f64 = 15.0;

/// The longest label drawn. A callsign is short; a remark pasted into one is not.
const LABEL_CHARS: usize = 28;

const ALERT: &str = "#d92d20";
const DRAWING: &str = "#c2410c";
const MARKER: &str = "#344054";

/// One feature as `js/map.js` takes it: `{ uid, anchor, shape }`.
pub fn draw(feature: &MapFeature, now: DateTime<Utc>) -> Value {
    let stale = feature.stale < now;
    let color = color(feature);

    let mut properties = json!({ "uid": feature.uid, "stale": stale, "color": color });
    match icon(feature) {
        Some(icon) => {
            properties["render"] = json!("symbol");
            properties["icon"] = json!(icon);
        }
        None => properties["render"] = json!("dot"),
    }
    if let Some(label) = label(feature) {
        properties["label"] = json!(format!("label:{label}"));
    }

    json!({
        "uid": feature.uid,
        "anchor": {
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [feature.point.lon, feature.point.lat] },
            "properties": properties,
        },
        "shape": feature.shape.as_ref().map(|shape| json!({
            "type": "Feature",
            "geometry": shape,
            "properties": { "uid": feature.uid, "stale": stale, "color": color },
        })),
    })
}

/// The image a symbol is drawn with: `sidc:<code>[:<direction>[:<fallback>]]`.
/// [`None`] for anything drawn as a dot.
///
/// The code is the one the sender asked for in `<__milicon>` or `<__milsym>`
/// when it wrote one, in whichever edition of MIL-STD-2525 it wrote it, and the
/// letter code its CoT type implies when it did not. A sender's code is
/// followed by the type's as a fallback, for a code the drawing library has no
/// picture for; a direction of 0 is "none", which is what lets a fallback
/// follow a track that is standing still.
fn icon(feature: &MapFeature) -> Option<String> {
    if feature.team.is_some() {
        return None;
    }

    let implied = sidc::from_cot_type(&feature.kind);
    let moving = feature.speed.is_some_and(|speed| speed > MOVING);
    let direction = feature.course.filter(|_| moving).map(direction);

    let asked = feature.sidc.as_deref();
    let code = asked.or(implied.as_deref())?;

    // Only a sender's code needs the type's behind it; the type's own is
    // already the last word.
    Some(
        match (implied.as_deref().filter(|_| asked.is_some()), direction) {
            (Some(implied), direction) => {
                format!("sidc:{code}:{}:{implied}", direction.unwrap_or(0))
            }
            (None, Some(direction)) => format!("sidc:{code}:{direction}"),
            (None, None) => format!("sidc:{code}"),
        },
    )
}

/// A course as the arrow that is drawn for it: a multiple of the step in
/// `1..=360`. North is 360 rather than 0, which the JavaScript reads as "none".
fn direction(course: f64) -> u32 {
    let stepped = (course.rem_euclid(360.0) / DIRECTION_STEP).round() * DIRECTION_STEP;

    match stepped as u32 % 360 {
        0 => 360,
        degrees => degrees,
    }
}

fn label(feature: &MapFeature) -> Option<String> {
    let callsign = feature.callsign.as_deref()?.trim();

    (!callsign.is_empty()).then(|| callsign.chars().take(LABEL_CHARS).collect())
}

fn color(feature: &MapFeature) -> &'static str {
    match &feature.team {
        Some(team) => team_color(team),
        None if feature.kind.starts_with("b-a-") => ALERT,
        None if feature.shape.is_some() => DRAWING,
        None => MARKER,
    }
}

/// The colour a TAK team name stands for. The names are the ones every TAK
/// client offers; one nobody has heard of is drawn as a plain marker.
pub fn team_color(team: &str) -> &'static str {
    match team.to_ascii_lowercase().as_str() {
        "white" => "#ffffff",
        "yellow" => "#ffd60a",
        "orange" => "#ff7a00",
        "magenta" => "#ff2bd6",
        "red" => "#e5252a",
        "maroon" => "#7f0000",
        "purple" => "#7f1fa8",
        "dark blue" => "#0a2a8f",
        "blue" => "#1e63ff",
        "cyan" => "#1ec8ff",
        "teal" => "#0f8a8a",
        "green" => "#21c44a",
        "dark green" => "#0b6b2a",
        "brown" => "#8a5a2b",
        _ => MARKER,
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::{MapPoint, MapShape};

    use super::*;

    pub(crate) fn feature(kind: &str) -> MapFeature {
        MapFeature {
            uid: "UID-1".to_string(),
            kind: kind.to_string(),
            how: None,
            callsign: Some("ALPHA".to_string()),
            team: None,
            role: None,
            time: "2026-09-18T12:00:00Z".parse().unwrap(),
            stale: "2026-09-18T12:02:00Z".parse().unwrap(),
            received_at: "2026-09-18T12:00:00Z".parse().unwrap(),
            point: MapPoint {
                lat: 51.5,
                lon: -0.12,
                hae: None,
                ce: None,
                le: None,
            },
            shape: None,
            course: None,
            speed: None,
            battery: None,
            remarks: None,
            software: None,
            sidc: None,
            groups: Vec::new(),
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-09-18T12:01:00Z".parse().unwrap()
    }

    #[test]
    fn an_atom_is_its_symbol_and_somebody_on_a_team_is_a_dot_in_its_colour() {
        let hostile = draw(&feature("a-h-G-U-C"), now());
        assert_eq!(hostile["anchor"]["properties"]["render"], "symbol");
        assert_eq!(
            hostile["anchor"]["properties"]["icon"],
            "sidc:SHGPUC---------"
        );

        let teammate = draw(
            &MapFeature {
                team: Some("Cyan".to_string()),
                ..feature("a-f-G-U-C")
            },
            now(),
        );
        assert_eq!(teammate["anchor"]["properties"]["render"], "dot");
        assert_eq!(
            teammate["anchor"]["properties"]["color"],
            team_color("cyan")
        );
    }

    #[test]
    fn a_code_the_sender_asked_for_is_drawn_with_the_types_own_behind_it() {
        const ASKED: &str = "10060100001102000000";
        let asked = |kind: &str, course, speed| {
            icon(&MapFeature {
                sidc: Some(ASKED.to_string()),
                course,
                speed,
                ..feature(kind)
            })
        };

        assert_eq!(
            asked("a-h-A-M-H", None, None).as_deref(),
            Some("sidc:10060100001102000000:0:SHAPMH---------")
        );
        assert_eq!(
            asked("a-h-A-M-H", Some(92.0), Some(60.0)).as_deref(),
            Some("sidc:10060100001102000000:90:SHAPMH---------")
        );
        // A type that implies no symbol of its own still draws the one asked for.
        assert_eq!(
            asked("b-m-p-s-m", None, None).as_deref(),
            Some("sidc:10060100001102000000")
        );
        assert_eq!(icon(&feature("b-m-p-s-m")), None);
    }

    #[test]
    fn the_anchor_is_geojson_so_longitude_comes_first() {
        let drawn = draw(&feature("b-m-p-s-m"), now());

        assert_eq!(
            drawn["anchor"]["geometry"]["coordinates"],
            json!([-0.12, 51.5])
        );
        assert_eq!(drawn["anchor"]["properties"]["label"], "label:ALPHA");
        assert_eq!(drawn["shape"], Value::Null);
    }

    #[test]
    fn only_something_moving_gets_an_arrow_and_north_is_not_nothing() {
        let moving = |course, speed| {
            icon(&MapFeature {
                course: Some(course),
                speed: Some(speed),
                ..feature("a-f-A-C-F")
            })
            .unwrap()
        };

        assert_eq!(moving(92.0, 120.0), "sidc:SFAPCF---------:90");
        assert_eq!(moving(359.0, 120.0), "sidc:SFAPCF---------:360");
        assert_eq!(moving(92.0, 0.1), "sidc:SFAPCF---------");
    }

    #[test]
    fn staleness_is_decided_against_the_clock_it_is_drawn_at() {
        let late = "2026-09-18T12:03:00Z".parse().unwrap();

        assert_eq!(
            draw(&feature("a-f-G"), now())["anchor"]["properties"]["stale"],
            false
        );
        assert_eq!(
            draw(&feature("a-f-G"), late)["anchor"]["properties"]["stale"],
            true
        );
    }

    #[test]
    fn a_drawing_brings_its_outline() {
        let drawn = draw(
            &MapFeature {
                shape: Some(MapShape::LineString(vec![[-0.12, 51.5], [-0.13, 51.6]])),
                ..feature("u-d-f")
            },
            now(),
        );

        assert_eq!(drawn["shape"]["geometry"]["type"], "LineString");
        assert_eq!(drawn["shape"]["properties"]["uid"], "UID-1");
    }
}
