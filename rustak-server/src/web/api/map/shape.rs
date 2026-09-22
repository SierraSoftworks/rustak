//! The outline of a drawing, read out of `<detail>`.
//!
//! TAK clients spell a shape two ways. A freeform line, a polygon, a rectangle
//! and a route are a run of `<link point="lat,lon">` elements, in order; a
//! circle and an ellipse are a `<shape><ellipse major minor angle>` around the
//! event's own point. Both are written here from the elements as clients are
//! observed to send them, and both come out as GeoJSON — so `[lon, lat]`, and a
//! polygon whose ring repeats its first position.

use rustak_api::MapShape;
use rustak_cot::Event;
use rustak_cot::detail::Element;

/// How many sides a circle is drawn with.
const ELLIPSE_STEPS: usize = 64;

/// Metres in a degree of latitude, near enough for something drawn by hand.
const METRES_PER_DEGREE: f64 = 111_320.0;

/// The shape an event draws, if it draws one.
pub fn of(event: &Event) -> Option<MapShape> {
    let ellipse = event
        .detail
        .find("shape")
        .and_then(|shape| shape.child("ellipse"));

    match ellipse {
        Some(ellipse) => self::ellipse(event.point.lat, event.point.lon, ellipse),
        None => linked(event),
    }
}

/// A line or a polygon from a run of `<link point>`.
fn linked(event: &Event) -> Option<MapShape> {
    let mut positions: Vec<[f64; 2]> = event
        .detail
        .find_all("link")
        .into_iter()
        .filter_map(|link| link.get("point"))
        .filter_map(position)
        .collect();

    if positions.len() < 2 {
        return None;
    }

    // A rectangle is four corners and is closed by definition; anything else
    // is closed when its author closed it.
    let rectangle = event.r#type.starts_with("u-d-r") && positions.len() >= 3;
    if rectangle && positions.first() != positions.last() {
        positions.push(positions[0]);
    }

    match positions.first() == positions.last() && positions.len() >= 4 {
        true => Some(MapShape::Polygon(vec![positions])),
        false => Some(MapShape::LineString(positions)),
    }
}

/// `lat,lon[,hae]` as a GeoJSON position.
fn position(point: &str) -> Option<[f64; 2]> {
    let mut parts = point.split(',').map(|part| part.trim().parse::<f64>());
    let lat = parts.next()?.ok()?;
    let lon = parts.next()?.ok()?;

    (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some([lon, lat])
}

/// An ellipse as a polygon: semi-axes in metres, `angle` the major axis's
/// bearing in degrees clockwise from north.
fn ellipse(lat: f64, lon: f64, ellipse: &Element) -> Option<MapShape> {
    let read = |name: &str| {
        ellipse
            .get(name)
            .and_then(|value| value.parse::<f64>().ok())
    };
    let (major, minor) = (read("major")?, read("minor")?);
    let bearing = read("angle").unwrap_or(0.0).to_radians();

    if !(major.is_finite() && minor.is_finite() && major > 0.0 && minor > 0.0) {
        return None;
    }

    let metres_per_degree_lon = METRES_PER_DEGREE * lat.to_radians().cos().abs().max(1e-6);

    let mut ring: Vec<[f64; 2]> = (0..ELLIPSE_STEPS)
        .map(|step| {
            let around = std::f64::consts::TAU * step as f64 / ELLIPSE_STEPS as f64;
            let (along, across) = (major * around.cos(), minor * around.sin());
            let north = along * bearing.cos() - across * bearing.sin();
            let east = along * bearing.sin() + across * bearing.cos();

            [
                lon + east / metres_per_degree_lon,
                lat + north / METRES_PER_DEGREE,
            ]
        })
        .collect();
    ring.push(ring[0]);

    Some(MapShape::Polygon(vec![ring]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawing(kind: &str, points: &[&str]) -> Event {
        points
            .iter()
            .fold(
                Event::builder(kind, "SHAPE-1").point(51.5, -0.12),
                |event, point| event.push(Element::new("link").attr("point", *point)),
            )
            .build()
    }

    #[test]
    fn an_open_run_of_links_is_a_line_in_geojson_order() {
        let shape = of(&drawing("b-m-r", &["51.5,-0.12", "51.6,-0.13,35.0"]));

        assert_eq!(
            shape,
            Some(MapShape::LineString(vec![[-0.12, 51.5], [-0.13, 51.6]]))
        );
    }

    #[test]
    fn a_run_that_returns_to_its_start_is_a_polygon() {
        let corners = ["51.5,-0.12", "51.6,-0.12", "51.6,-0.13", "51.5,-0.12"];

        assert!(matches!(
            of(&drawing("u-d-f", &corners)),
            Some(MapShape::Polygon(rings)) if rings[0].len() == 4
        ));
    }

    #[test]
    fn a_rectangle_is_closed_whether_or_not_its_author_closed_it() {
        let corners = ["51.5,-0.12", "51.6,-0.12", "51.6,-0.13", "51.5,-0.13"];

        assert!(matches!(
            of(&drawing("u-d-r", &corners)),
            Some(MapShape::Polygon(rings)) if rings[0].first() == rings[0].last()
        ));
    }

    #[test]
    fn a_circle_is_a_closed_ring_around_the_events_own_point() {
        let event = Event::builder("u-d-c-c", "CIRCLE-1")
            .point(51.5, -0.12)
            .push(
                Element::new("shape").with(
                    Element::new("ellipse")
                        .attr("major", "1000")
                        .attr("minor", "1000")
                        .attr("angle", "360"),
                ),
            )
            .build();

        let Some(MapShape::Polygon(rings)) = of(&event) else {
            panic!("a circle draws a polygon");
        };

        assert_eq!(rings[0].first(), rings[0].last());
        // A kilometre north of the centre, give or take the approximation.
        let northmost = rings[0].iter().map(|at| at[1]).fold(f64::MIN, f64::max);
        assert!((northmost - 51.509).abs() < 0.001, "{northmost}");
    }

    #[test]
    fn an_ellipse_with_no_size_draws_nothing() {
        for (major, minor) in [("0", "100"), ("100", "-1"), ("NaN", "100"), ("wide", "100")] {
            let event = Event::builder("u-d-c-c", "CIRCLE-1")
                .point(51.5, -0.12)
                .push(
                    Element::new("shape").with(
                        Element::new("ellipse")
                            .attr("major", major)
                            .attr("minor", minor),
                    ),
                )
                .build();

            assert_eq!(of(&event), None, "major={major} minor={minor}");
        }
    }

    #[test]
    fn a_marker_with_nothing_to_outline_has_no_shape() {
        assert_eq!(of(&drawing("a-f-G-U-C", &[])), None);
        assert_eq!(of(&drawing("u-d-f", &["not,a point", "51.5,-0.12"])), None);
        // Off the planet, and half a position.
        assert_eq!(of(&drawing("u-d-f", &["91.0,-0.12", "51.5"])), None);
    }
}
