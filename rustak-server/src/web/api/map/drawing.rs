//! A drawing made in the console, written as the `<detail>` ATAK's own
//! drawing tools write.
//!
//! Only what ATAK draws natively, under the types it draws it as: a line or a
//! polygon is a `u-d-f`, a rectangle a `u-d-r`, a circle a `u-d-c-c` and a
//! route a `b-m-r`. The outline is a run of `<link point="lat,lon">` — a
//! polygon's closed by repeating its first, a rectangle's four corners left
//! open, a route's each a waypoint with a uid of its own — or, for a circle,
//! a `<shape><ellipse>` around the event's point. [`shape`](super::shape)
//! reads all of it back, so what the console drew and what it then shows are
//! the same message a device was sent.

use rustak_api::map::rgb;
use rustak_api::{MapShape, MapStyle, PublishFeature};
use rustak_cot::detail::{Element, Node};

const FREEFORM: &str = "u-d-f";
const RECTANGLE: &str = "u-d-r";
const CIRCLE: &str = "u-d-c-c";
const ROUTE: &str = "b-m-r";

/// The most positions an outline may have. A message has to fit a datagram
/// on the networks TAK runs over, and nobody draws this many by hand.
const MAX_POSITIONS: usize = 500;

/// The widest line worth asking for, as TAK clients count widths.
const MAX_WEIGHT: f64 = 20.0;

/// The largest circle: half way round the world.
const MAX_RADIUS: f64 = 20_000_000.0;

/// What is wrong with a draft's outline, in words somebody can act on.
///
/// # Errors
///
/// An outline that does not belong to the type it is published under, that is
/// too short or too long to be one, or that is drawn in something that is not
/// a colour.
pub fn check(draft: &PublishFeature) -> Result<(), String> {
    match (draft.kind.as_str(), &draft.shape, &draft.ellipse) {
        (FREEFORM | ROUTE, Some(MapShape::LineString(line)), None) => open(line)?,
        (FREEFORM, Some(MapShape::Polygon(rings)), None) => closed(rings, 4..=MAX_POSITIONS)?,
        (RECTANGLE, Some(MapShape::Polygon(rings)), None) => closed(rings, 5..=5)?,
        (CIRCLE, None, Some(ellipse)) => {
            let round = ellipse.major == ellipse.minor
                && ellipse.major > 0.0
                && ellipse.major <= MAX_RADIUS;
            if !round {
                return Err("A circle has one radius, in metres, and it is more than none.".into());
            }
        }
        (FREEFORM | RECTANGLE | CIRCLE | ROUTE, _, _) => {
            return Err(
                "A drawing is published with its outline: a line for a route, a line or a \
                 closed ring for a shape, a ring of four corners for a rectangle, and a \
                 radius for a circle."
                    .into(),
            );
        }
        (_, None, None) => {}
        _ => return Err("Only a drawing (u-d-f, u-d-r, u-d-c-c or b-m-r) has an outline.".into()),
    }

    draft.style.as_ref().map_or(Ok(()), styled)
}

fn open(line: &[[f64; 2]]) -> Result<(), String> {
    match (2..=MAX_POSITIONS).contains(&line.len()) && line.iter().all(placed) {
        true => Ok(()),
        false => Err(format!(
            "A line is between 2 and {MAX_POSITIONS} positions, each of them on the planet."
        )),
    }
}

fn closed(rings: &[Vec<[f64; 2]>], length: std::ops::RangeInclusive<usize>) -> Result<(), String> {
    let ring = match rings {
        [ring] if length.contains(&ring.len()) && ring.first() == ring.last() => ring,
        _ => {
            return Err(format!(
                "An area is one ring of {} to {} positions, the last repeating the first.",
                length.start(),
                length.end()
            ));
        }
    };

    match ring.iter().all(placed) {
        true => Ok(()),
        false => Err("Every position of an outline is on the planet.".into()),
    }
}

fn placed(at: &[f64; 2]) -> bool {
    let [lon, lat] = *at;

    lon.is_finite() && lat.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0
}

fn styled(style: &MapStyle) -> Result<(), String> {
    let colours = [&style.stroke, &style.fill]
        .into_iter()
        .flatten()
        .all(|colour| rgb(colour).is_some());
    let weight = style
        .weight
        .is_none_or(|weight| weight > 0.0 && weight <= MAX_WEIGHT);
    let opacity = style
        .fill_opacity
        .is_none_or(|opacity| (0.0..=1.0).contains(&opacity));

    match colours && weight && opacity {
        true => Ok(()),
        false => Err(format!(
            "A colour is #rrggbb, a line's weight is up to {MAX_WEIGHT}, and an opacity is between 0 and 1."
        )),
    }
}

/// The elements that say what a draft draws, in the order ATAK writes them.
/// Nothing, for a draft that is only a point.
pub fn elements(uid: &str, draft: &PublishFeature) -> Vec<Element> {
    let style = draft.style.clone().unwrap_or_default();
    let stroke = style.stroke.as_deref().and_then(|colour| argb(colour, 1.0));
    let fill = style
        .fill
        .as_deref()
        .and_then(|colour| argb(colour, style.fill_opacity.unwrap_or(1.0)));
    let route = draft.kind == ROUTE;

    let mut elements: Vec<Element> = match (&draft.shape, &draft.ellipse) {
        (Some(MapShape::LineString(line)), _) if route => waypoints(uid, line),
        (Some(MapShape::LineString(line)), _) => line.iter().map(link).collect(),
        // A rectangle is its four corners; anything else closed says so by
        // ending where it began.
        (Some(MapShape::Polygon(rings)), _) => rings
            .first()
            .map(|ring| match draft.kind == RECTANGLE {
                true => &ring[..ring.len().saturating_sub(1)],
                false => &ring[..],
            })
            .unwrap_or_default()
            .iter()
            .map(link)
            .collect(),
        (None, Some(ellipse)) => vec![circle(uid, ellipse.major, stroke, fill)],
        (None, None) => return Vec::new(),
    };

    if route {
        elements.push(
            Element::new("link_attr")
                .attr("method", "Driving")
                .attr("direction", "Infil")
                .attr("routetype", "Primary")
                .attr("order", "Ascending Check Points")
                .attr("prefix", "CP")
                .attr_opt("color", stroke.map(|colour| colour.to_string()))
                .attr_opt("stroke", style.weight.map(|weight| format!("{weight:.0}"))),
        );
        elements.push(Element::new("__routeinfo").with(Element::new("__navcues")));
    }

    let valued = |name: &str, value: String| Element::new(name).attr("value", value);
    elements.extend(stroke.map(|colour| valued("strokeColor", colour.to_string())));
    elements.extend(
        style
            .weight
            .map(|weight| valued("strokeWeight", format!("{weight:.1}"))),
    );
    if !route && !matches!(draft.shape, Some(MapShape::LineString(_))) {
        elements.extend(fill.map(|colour| valued("fillColor", colour.to_string())));
    }
    if draft.kind == RECTANGLE {
        elements.push(Element::new("tog").attr("enabled", "0"));
    }
    elements.push(valued("labels_on", "true".to_string()));

    elements
}

/// `<link point="lat,lon"/>`, which is CoT's order and the opposite of the
/// position's.
fn link(at: &[f64; 2]) -> Element {
    Element::new("link").attr("point", format!("{},{}", at[1], at[0]))
}

/// A route's positions as ATAK writes them: each a point of its own, the ends
/// waypoints somebody is told about and the bends between them control points
/// nobody is.
fn waypoints(uid: &str, line: &[[f64; 2]]) -> Vec<Element> {
    let last = line.len().saturating_sub(1);

    line.iter()
        .enumerate()
        .map(|(index, at)| {
            let (kind, callsign) = match index {
                0 => ("b-m-p-w", "SP".to_string()),
                _ if index == last => ("b-m-p-w", "VDO".to_string()),
                _ => ("b-m-p-c", String::new()),
            };

            Element::new("link")
                .attr("uid", format!("{uid}-{index}"))
                .attr("callsign", callsign)
                .attr("type", kind)
                .attr("point", format!("{},{}", at[1], at[0]))
                .attr("remarks", "")
                .attr("relation", "c")
        })
        .collect()
}

/// `<shape><ellipse>` with the KML style ATAK writes inside it, which is
/// where some clients look for a circle's colours.
fn circle(uid: &str, radius: f64, stroke: Option<i32>, fill: Option<i32>) -> Element {
    let hex = |colour: i32| Node::Text(format!("{:08x}", colour as u32));
    let mut kml = Element::new("Style");
    if let Some(stroke) = stroke {
        kml.push(Element::new("LineStyle").with(Element::new("color").with(hex(stroke))));
    }
    if let Some(fill) = fill {
        kml.push(Element::new("PolyStyle").with(Element::new("color").with(hex(fill))));
    }

    Element::new("shape")
        .with(
            Element::new("ellipse")
                .attr("major", radius.to_string())
                .attr("minor", radius.to_string())
                .attr("angle", "360"),
        )
        .with(
            Element::new("link")
                .attr("uid", format!("{uid}.Style"))
                .attr("type", "b-x-KmlStyle")
                .attr("relation", "p-c")
                .with(kml),
        )
}

/// `#rrggbb` at an opacity, as CoT spells a colour: ARGB, as a signed 32-bit
/// integer.
fn argb(colour: &str, opacity: f64) -> Option<i32> {
    let [red, green, blue] = rgb(colour)?;
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;

    Some(i32::from_be_bytes([alpha, red, green, blue]))
}

#[cfg(test)]
mod tests {
    use rustak_api::{MapEllipse, MapPoint};
    use rustak_cot::Event;

    use super::super::shape;
    use super::*;

    const SQUARE: [[f64; 2]; 5] = [
        [-0.12, 51.5],
        [-0.11, 51.5],
        [-0.11, 51.51],
        [-0.12, 51.51],
        [-0.12, 51.5],
    ];

    fn draft(kind: &str, shape: Option<MapShape>, ellipse: Option<MapEllipse>) -> PublishFeature {
        PublishFeature {
            kind: kind.to_string(),
            callsign: "DRAWING".to_string(),
            point: MapPoint {
                lat: 51.505,
                lon: -0.115,
                hae: None,
                ce: None,
                le: None,
            },
            how: Some("h-e".to_string()),
            remarks: None,
            sidc: None,
            stale: None,
            groups: Vec::new(),
            shape,
            ellipse,
            style: Some(MapStyle {
                stroke: Some("#ff0000".to_string()),
                weight: Some(4.0),
                fill: Some("#ff0000".to_string()),
                fill_opacity: Some(150.0 / 255.0),
            }),
        }
    }

    fn line() -> MapShape {
        MapShape::LineString(SQUARE[..3].to_vec())
    }

    fn area() -> MapShape {
        MapShape::Polygon(vec![SQUARE.to_vec()])
    }

    fn circle() -> MapEllipse {
        MapEllipse {
            major: 250.0,
            minor: 250.0,
            angle: 0.0,
        }
    }

    /// The draft as an event, the way `publish` builds one.
    fn written(draft: &PublishFeature) -> Event {
        elements("DRAWING-1", draft)
            .into_iter()
            .fold(
                Event::builder(draft.kind.as_str(), "DRAWING-1")
                    .point(draft.point.lat, draft.point.lon),
                |event, element| event.push(element),
            )
            .build()
    }

    #[test]
    fn what_is_written_reads_back_as_what_was_drawn() {
        for (kind, outline, ellipse) in [
            ("u-d-f", Some(line()), None),
            ("u-d-f", Some(area()), None),
            ("u-d-r", Some(area()), None),
            ("b-m-r", Some(line()), None),
            ("u-d-c-c", None, Some(circle())),
        ] {
            let draft = draft(kind, outline.clone(), ellipse);
            assert_eq!(check(&draft), Ok(()), "{kind}");

            let event = written(&draft);
            match outline {
                Some(outline) => assert_eq!(shape::of(&event), Some(outline), "{kind}"),
                // ATAK writes a circle's bearing as 360, which is north.
                None => assert_eq!(
                    shape::ellipse(&event).map(|read| read.major),
                    ellipse.map(|drawn| drawn.major),
                    "{kind}"
                ),
            }

            let style = shape::style(&event).expect("it says how it is drawn");
            assert_eq!(style.stroke.as_deref(), Some("#ff0000"), "{kind}");
            assert_eq!(style.weight, Some(4.0), "{kind}");
            assert_eq!(
                style.fill.is_some(),
                matches!(shape::of(&event), Some(MapShape::Polygon(_))),
                "{kind}: only an area is filled"
            );
        }
    }

    #[test]
    fn a_route_is_written_as_the_waypoints_atak_navigates_by() {
        let event = written(&draft("b-m-r", Some(line()), None));
        let links = event.detail.find_all("link");

        let kinds: Vec<_> = links.iter().filter_map(|link| link.get("type")).collect();
        assert_eq!(kinds, ["b-m-p-w", "b-m-p-c", "b-m-p-w"]);
        assert!(links.iter().all(|link| link.get("relation") == Some("c")));
        assert!(links.iter().all(|link| link.get("uid").is_some()));
        assert!(event.detail.find("link_attr").is_some());
    }

    #[test]
    fn an_outline_that_is_not_one_is_refused() {
        let styled = |style: MapStyle| PublishFeature {
            style: Some(style),
            ..draft("u-d-f", Some(line()), None)
        };

        for (why, draft) in [
            (
                "a marker has no outline",
                draft("a-u-G", Some(line()), None),
            ),
            ("a drawing has one", draft("u-d-f", None, None)),
            (
                "a circle is not a ring",
                draft("u-d-c-c", Some(area()), None),
            ),
            ("a route is not an area", draft("b-m-r", Some(area()), None)),
            (
                "a line has two ends",
                draft("u-d-f", Some(MapShape::LineString(vec![[0.0, 0.0]])), None),
            ),
            (
                "a ring is closed",
                draft(
                    "u-d-f",
                    Some(MapShape::Polygon(vec![SQUARE[..4].to_vec()])),
                    None,
                ),
            ),
            (
                "a rectangle has four corners",
                draft(
                    "u-d-r",
                    Some(MapShape::Polygon(vec![vec![
                        SQUARE[0], SQUARE[1], SQUARE[2], SQUARE[0],
                    ]])),
                    None,
                ),
            ),
            (
                "a position is on the planet",
                draft(
                    "u-d-f",
                    Some(MapShape::LineString(vec![[0.0, 0.0], [0.0, 91.0]])),
                    None,
                ),
            ),
            (
                "a circle has a radius",
                draft(
                    "u-d-c-c",
                    None,
                    Some(MapEllipse {
                        major: 0.0,
                        minor: 0.0,
                        angle: 0.0,
                    }),
                ),
            ),
            (
                "a colour is a colour",
                styled(MapStyle {
                    stroke: Some("red".to_string()),
                    ..MapStyle::default()
                }),
            ),
            (
                "an opacity is a fraction",
                styled(MapStyle {
                    fill_opacity: Some(1.5),
                    ..MapStyle::default()
                }),
            ),
        ] {
            assert!(check(&draft).is_err(), "{why}");
        }
    }

    #[test]
    fn a_marker_is_left_as_it_was() {
        let marker = PublishFeature {
            style: None,
            ..draft("a-u-G", None, None)
        };

        assert_eq!(check(&marker), Ok(()));
        assert!(elements("MARKER-1", &marker).is_empty());
    }
}
