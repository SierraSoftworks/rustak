//! Turning a detection into what goes on the map.
//!
//! A detection can be drawn two ways, and `[settings.display] shape` chooses:
//!
//! | `shape` | What is published |
//! |---|---|
//! | `marker` | One point per detection, at the pixel centre |
//! | `footprint` | One polygon per detection: the ground the pixel covered |
//! | `both` | Both, under related uids |
//!
//! # The marker
//!
//! A spot-map marker (`b-m-p-s-m`) by default, because every TAK client draws
//! one as a dot in the `<color>` it is given, which is what a fire map is. The
//! type is a setting, for a deployment that would rather have a symbol.
//!
//! # The footprint
//!
//! A closed `u-d-f` drawing. `scan` is laid east-west and `track` north-south
//! about the pixel centre. **That is an approximation**: a polar orbiter's
//! scan line is tilted some ten degrees from east-west, and FIRMS does not
//! publish the angle. The area is right and the corners are close; this is a
//! sensor pixel, not a fire perimeter, and the remarks say so by giving its
//! size. A detection with no `scan`/`track` has no footprint to draw and is
//! published as a marker instead, whatever `shape` says.
//!
//! # The uid
//!
//! `FIRMS-<satellite>-<yyyymmddHHMM>-<lat>-<lon>`, position in 1e-5 degrees.
//! Deterministic, so the same detection read on the next poll, or by a sidecar
//! that restarted, overwrites itself on the map instead of stacking up.

use std::time::Duration;

use rustak_core::prelude::*;
use rustak_cot::detail::{Contact, Element, Remarks};
use rustak_cot::{CotTime, Event, Point};

use crate::wire::{Confidence, Detection};

/// The uid prefix every event from this plugin carries.
pub const PREFIX: &str = "FIRMS-";

/// What a footprint's uid adds to its marker's.
pub const FOOTPRINT_SUFFIX: &str = "-FP";

/// The spot-map marker: a coloured dot in every TAK client.
pub const SPOT_TYPE: &str = "b-m-p-s-m";

/// A freehand drawing, which is what a closed polygon is on the wire.
pub const FOOTPRINT_TYPE: &str = "u-d-f";

/// Machine-generated, as every feed in this workspace says.
const HOW: &str = "m-g";

/// Kilometres in a degree of latitude.
const KM_PER_DEGREE: f64 = 111.32;

/// How opaque a footprint's fill is, as the alpha byte of its colour.
const FILL_ALPHA: u32 = 0x5000_0000;

/// What is drawn for each detection.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// A point at the pixel centre.
    #[default]
    Marker,
    /// The ground the pixel covered, as a polygon.
    Footprint,
    /// Both.
    Both,
}

/// What a detection's colour says.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ColourBy {
    /// How sure the algorithm was: yellow, orange, red.
    #[default]
    Confidence,
    /// Fire radiative power: yellow under 10 MW, orange under 50, red under
    /// 100, dark red above.
    Intensity,
}

/// `[settings.display]` — how detections are drawn.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// Markers, footprints or both. Default: `marker`.
    #[serde(default)]
    pub shape: Shape,

    /// The CoT type of a marker. Default: `b-m-p-s-m`.
    #[serde(default = "default_marker_type")]
    pub marker_type: String,

    /// What the colour encodes. Default: `confidence`.
    #[serde(default)]
    pub colour_by: ColourBy,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            shape: Shape::default(),
            marker_type: default_marker_type(),
            colour_by: ColourBy::default(),
        }
    }
}

fn default_marker_type() -> String {
    SPOT_TYPE.to_string()
}

/// The uid this detection has on every map, every time it is read.
#[must_use]
pub fn uid(detection: &Detection) -> String {
    let satellite: String = detection
        .satellite
        .as_deref()
        .unwrap_or("X")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();

    format!(
        "{PREFIX}{}-{}-{}-{}",
        if satellite.is_empty() {
            "X"
        } else {
            &satellite
        },
        detection.acquired_at.format("%Y%m%d%H%M"),
        (detection.lat * 1e5).round() as i64,
        (detection.lon * 1e5).round() as i64,
    )
}

/// The events one detection becomes: one, or two for [`Shape::Both`].
///
/// `time` and `start` are the acquisition, and `stale` is `max_age` after it,
/// so a client ages a detection from when the satellite saw it rather than
/// from when this sidecar happened to say so.
#[must_use]
pub fn events(detection: &Detection, display: &Display, max_age: Duration) -> Vec<Event> {
    let uid = uid(detection);
    let colour = colour(detection, display.colour_by);
    let footprint = match display.shape {
        Shape::Marker => None,
        Shape::Footprint | Shape::Both => corners(detection),
    };

    let mut events = Vec::with_capacity(2);

    // A detection with no pixel size has no footprint, and is never dropped
    // for it: the marker is the fallback.
    if display.shape != Shape::Footprint || footprint.is_none() {
        let marker_type = match display.marker_type.trim() {
            "" => SPOT_TYPE,
            written => written,
        };

        events.push(event(
            marker_type,
            uid.clone(),
            detection,
            max_age,
            vec![Element::new("color").attr("argb", argb(colour))],
        ));
    }

    if let Some(corners) = footprint {
        // Closed by repeating the first vertex, which is how a client knows
        // this is an area and not a line.
        let mut details: Vec<Element> = corners
            .iter()
            .chain(corners.first())
            .map(|(lat, lon)| {
                Element::new("link")
                    .attr("relation", "c")
                    .attr("point", format!("{lat:.6},{lon:.6}"))
            })
            .collect();

        details.push(Element::new("strokeColor").attr("value", argb(colour)));
        details.push(Element::new("strokeWeight").attr("value", "2.0"));
        details.push(
            Element::new("fillColor").attr("value", argb((colour & 0x00FF_FFFF) | FILL_ALPHA)),
        );
        details.push(Element::new("labels_on").attr("value", "false"));

        events.push(event(
            FOOTPRINT_TYPE,
            format!("{uid}{FOOTPRINT_SUFFIX}"),
            detection,
            max_age,
            details,
        ));
    }

    events
}

/// The four corners of the ground this pixel covered — north-west, north-east,
/// south-east, south-west — or [`None`] when FIRMS did not say how big it was.
#[must_use]
pub fn corners(detection: &Detection) -> Option<[(f64, f64); 4]> {
    let (scan, track) = (detection.scan_km?, detection.track_km?);
    let (lat, lon) = (detection.lat, detection.lon);

    let half_lat = track / 2.0 / KM_PER_DEGREE;
    // A degree of longitude shrinks with the cosine of the latitude; the floor
    // keeps a pixel at a pole from wrapping the planet.
    let half_lon = scan / 2.0 / (KM_PER_DEGREE * lat.to_radians().cos().max(0.01));

    Some([
        (lat + half_lat, lon - half_lon),
        (lat + half_lat, lon + half_lon),
        (lat - half_lat, lon + half_lon),
        (lat - half_lat, lon - half_lon),
    ])
}

/// One event about this detection, with whatever details its shape adds.
fn event(
    r#type: &str,
    uid: String,
    detection: &Detection,
    max_age: Duration,
    details: Vec<Element>,
) -> Event {
    let time = CotTime::from_datetime(detection.acquired_at);
    let mut builder = Event::builder(r#type, uid)
        .how(HOW)
        .point_full(Point {
            ce: circular_error_m(detection).unwrap_or(Point::UNKNOWN_CE),
            ..Point::new(detection.lat, detection.lon)
        })
        .time(time)
        .start(time)
        .stale(time.stale_after(max_age))
        // No endpoint: a fire is a thing on the map, not a chat peer.
        .typed(&Contact::new(callsign(detection)))
        .typed(&Remarks {
            text: remarks(detection),
            ..Remarks::default()
        });

    for detail in details {
        builder = builder.push(detail);
    }

    builder.build()
}

/// What the map shows beside the dot: when, and how hard it is burning.
fn callsign(detection: &Detection) -> String {
    let seen = detection.acquired_at.format("%H:%MZ");

    match detection.frp_mw {
        Some(frp) => format!("Fire {seen} {frp:.0}MW"),
        None => format!("Fire {seen}"),
    }
}

/// The fire is somewhere in the pixel, so half its longer side is the honest
/// error on the point, in metres.
fn circular_error_m(detection: &Detection) -> Option<f64> {
    let longest = detection.scan_km?.max(detection.track_km?);

    Some(longest * 1_000.0 / 2.0)
}

/// The ordered `key: value` lines an operator reads when they tap a detection.
fn remarks(detection: &Detection) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!(
        "Detected: {}",
        detection.acquired_at.format("%Y-%m-%d %H:%M UTC"),
    ));
    match (&detection.satellite, &detection.instrument) {
        (Some(satellite), Some(instrument)) => {
            lines.push(format!("Satellite: {satellite} ({instrument})"));
        }
        (Some(name), None) | (None, Some(name)) => lines.push(format!("Satellite: {name}")),
        (None, None) => {}
    }
    if let Some(confidence) = detection.confidence {
        lines.push(format!("Confidence: {}", confidence.label()));
    }
    if let Some(frp) = detection.frp_mw {
        lines.push(format!("FRP: {frp:.1} MW"));
    }
    if let Some(brightness) = detection.brightness_k {
        lines.push(format!("Brightness: {brightness:.1} K"));
    }
    if let (Some(scan), Some(track)) = (detection.scan_km, detection.track_km) {
        lines.push(format!("Pixel: {scan:.2} x {track:.2} km"));
    }
    if let Some(daytime) = detection.daytime {
        lines.push(format!(
            "Overpass: {}",
            if daytime { "day" } else { "night" }
        ));
    }
    match &detection.version {
        Some(version) => lines.push(format!("Source: NASA FIRMS ({version})")),
        None => lines.push("Source: NASA FIRMS".to_string()),
    }

    lines.join("\n")
}

const YELLOW: u32 = 0xFFFF_FF00;
const ORANGE: u32 = 0xFFFF_8C00;
const RED: u32 = 0xFFFF_0000;
const DARK_RED: u32 = 0xFF8B_0000;

/// This detection's colour, as `0xAARRGGBB`. What is not known is orange: the
/// middle of either scale, which claims the least.
fn colour(detection: &Detection, by: ColourBy) -> u32 {
    match by {
        ColourBy::Confidence => match detection.confidence {
            Some(Confidence::Low) => YELLOW,
            Some(Confidence::Nominal) | None => ORANGE,
            Some(Confidence::High) => RED,
        },
        ColourBy::Intensity => match detection.frp_mw {
            Some(frp) if frp < 10.0 => YELLOW,
            Some(frp) if frp < 50.0 => ORANGE,
            Some(frp) if frp < 100.0 => RED,
            Some(_) => DARK_RED,
            None => ORANGE,
        },
    }
}

/// A colour the way CoT writes one: the same 32 bits, read as a signed integer.
fn argb(colour: u32) -> String {
    i32::from_be_bytes(colour.to_be_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    fn detection() -> Detection {
        Detection {
            satellite: Some("N20".into()),
            instrument: Some("VIIRS".into()),
            confidence: Some(Confidence::High),
            frp_mw: Some(47.3),
            scan_km: Some(0.4),
            track_km: Some(0.4),
            ..Detection::at(40.10234, -7.91456, "2026-09-22T13:42:00Z".parse().unwrap())
        }
    }

    fn display(shape: Shape) -> Display {
        Display {
            shape,
            ..Display::default()
        }
    }

    fn wire(event: &Event) -> String {
        String::from_utf8(rustak_cot::xml::write(event).to_vec()).expect("CoT XML is UTF-8")
    }

    #[test]
    fn the_same_detection_always_has_the_same_uid() {
        assert_eq!(uid(&detection()), "FIRMS-N20-202609221342-4010234--791456");
        assert_eq!(uid(&detection()), uid(&detection().clone()));
    }

    #[test]
    fn a_marker_is_a_coloured_spot_aged_from_the_acquisition() {
        let published = events(&detection(), &display(Shape::Marker), DAY);
        let [marker] = published.as_slice() else {
            panic!("one marker, got {published:?}");
        };
        let xml = wire(marker);

        assert_eq!(marker.r#type, SPOT_TYPE);
        assert!(marker.uid.starts_with(PREFIX));
        assert_eq!(marker.time, CotTime::from_datetime(detection().acquired_at));
        assert_eq!(marker.stale.millis() - marker.time.millis(), 86_400_000);
        assert!(
            (marker.point.ce - 200.0).abs() < 1e-6,
            "half a 0.4 km pixel"
        );
        assert!(
            xml.contains(r#"argb="-65536""#),
            "high confidence is red: {xml}"
        );
        assert!(xml.contains("FRP: 47.3 MW"), "{xml}");
        assert!(xml.contains("Source: NASA FIRMS"), "attribution: {xml}");
        assert!(!xml.contains("endpoint="), "{xml}");
    }

    #[test]
    fn a_footprint_is_a_closed_polygon_around_the_pixel_centre() {
        let published = events(&detection(), &display(Shape::Footprint), DAY);
        let [footprint] = published.as_slice() else {
            panic!("one footprint, got {published:?}");
        };

        assert_eq!(footprint.r#type, FOOTPRINT_TYPE);
        assert!(footprint.uid.ends_with(FOOTPRINT_SUFFIX));

        let links = footprint.detail.find_all("link");

        assert_eq!(links.len(), 5, "four corners, and the first again");
        assert_eq!(links[0].get("point"), links[4].get("point"));

        let corners = corners(&detection()).expect("a pixel with a size");
        let north_south_km = (corners[0].0 - corners[3].0) * KM_PER_DEGREE;

        assert!((north_south_km - 0.4).abs() < 1e-6, "{north_south_km}");
        assert!(corners[0].1 < detection().lon && corners[1].1 > detection().lon);
    }

    #[test]
    fn both_shapes_share_a_uid_stem() {
        let published = events(&detection(), &display(Shape::Both), DAY);

        assert_eq!(published.len(), 2);
        assert_eq!(
            published[1].uid,
            format!("{}{FOOTPRINT_SUFFIX}", published[0].uid),
        );
    }

    #[test]
    fn a_detection_with_no_pixel_size_is_a_marker_whatever_was_asked_for() {
        let bare = Detection::at(1.0, 2.0, detection().acquired_at);

        for shape in [Shape::Marker, Shape::Footprint, Shape::Both] {
            let published = events(&bare, &display(shape), DAY);

            assert_eq!(published.len(), 1, "{shape:?}");
            assert_eq!(published[0].r#type, SPOT_TYPE, "{shape:?}");
        }
    }

    #[test]
    fn the_colour_follows_the_scale_the_operator_chose() {
        for (by, confidence, frp, expected) in [
            (ColourBy::Confidence, Some(Confidence::Low), None, YELLOW),
            (ColourBy::Confidence, None, None, ORANGE),
            (
                ColourBy::Intensity,
                Some(Confidence::High),
                Some(5.0),
                YELLOW,
            ),
            (ColourBy::Intensity, None, Some(250.0), DARK_RED),
        ] {
            let detection = Detection {
                confidence,
                frp_mw: frp,
                ..detection()
            };

            assert_eq!(
                colour(&detection, by),
                expected,
                "{by:?} {confidence:?} {frp:?}"
            );
        }
    }

    #[test]
    fn the_marker_type_is_the_operators_to_choose() {
        let display = Display {
            marker_type: "a-u-G".to_string(),
            ..Display::default()
        };

        assert_eq!(events(&detection(), &display, DAY)[0].r#type, "a-u-G");
    }
}
