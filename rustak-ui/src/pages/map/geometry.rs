//! What can be drawn, and the arithmetic of drawing it.
//!
//! Five forms, each of them something ATAK's own drawing tools make and
//! every TAK client draws: a line and a polygon (both `u-d-f`, the polygon
//! closed), a rectangle (`u-d-r`), a circle (`u-d-c-c`) and a route
//! (`b-m-r`). A [`Geometry`] is one of them as somebody drew it — the
//! vertices they clicked rather than the ring a map is handed — which is what
//! an editor moves and what is published. Positions are `[lon, lat]`
//! throughout. Nothing here touches the browser, so all of it is tested
//! natively.

use rustak_api::{MapEllipse, MapFeature, MapShape};

/// Metres in a degree of latitude, near enough for something drawn by hand.
/// The server draws a circle with the same number, so a preview and the ring
/// that comes back agree.
const METRES_PER_DEGREE: f64 = 111_320.0;

/// How many sides a circle is previewed with.
const CIRCLE_STEPS: usize = 64;

/// The mean radius of the Earth, in metres.
const EARTH_RADIUS: f64 = 6_371_008.8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Line,
    Polygon,
    Rectangle,
    Circle,
    Route,
}

impl Form {
    pub const ALL: [Self; 5] = [
        Self::Line,
        Self::Polygon,
        Self::Rectangle,
        Self::Circle,
        Self::Route,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Line => "Line",
            Self::Polygon => "Polygon",
            Self::Rectangle => "Rectangle",
            Self::Circle => "Circle",
            Self::Route => "Route",
        }
    }

    /// The CoT type ATAK publishes one as.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Line | Self::Polygon => "u-d-f",
            Self::Rectangle => "u-d-r",
            Self::Circle => "u-d-c-c",
            Self::Route => "b-m-r",
        }
    }

    /// Whether it encloses an area, and so is filled.
    pub fn closed(self) -> bool {
        matches!(self, Self::Polygon | Self::Rectangle | Self::Circle)
    }

    /// Whether it is drawn with two clicks — a corner and its opposite, a
    /// centre and its edge — rather than with as many as it takes.
    pub fn two_clicks(self) -> bool {
        matches!(self, Self::Rectangle | Self::Circle)
    }

    /// The fewest vertices that make one.
    pub fn least(self) -> usize {
        match self {
            Self::Polygon => 3,
            _ => 2,
        }
    }
}

/// A drawing as it was drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub form: Form,
    /// The vertices, in order: a polygon's without its first repeated, a
    /// rectangle's four corners, and a circle's centre alone.
    pub points: Vec<[f64; 2]>,
    /// A circle's radius in metres, and nothing otherwise.
    pub radius: f64,
}

impl Geometry {
    /// A rectangle square to the map, from two opposite corners.
    pub fn rectangle(a: [f64; 2], c: [f64; 2]) -> Self {
        Self {
            form: Form::Rectangle,
            points: vec![a, [c[0], a[1]], c, [a[0], c[1]]],
            radius: 0.0,
        }
    }

    pub fn circle(centre: [f64; 2], radius: f64) -> Self {
        Self {
            form: Form::Circle,
            points: vec![centre],
            radius,
        }
    }

    pub fn path(form: Form, points: Vec<[f64; 2]>) -> Self {
        Self {
            form,
            points,
            radius: 0.0,
        }
    }

    /// What a feature was drawn as, when it is one of the forms here exactly:
    /// a subtype, or an outline that is not what its type says, is somebody
    /// else's drawing to look at and not this console's to take apart.
    pub fn of(feature: &MapFeature) -> Option<Self> {
        let ring = |rings: &Vec<Vec<[f64; 2]>>| {
            let ring = rings.first()?;
            Some(ring[..ring.len().saturating_sub(1)].to_vec())
        };

        match (feature.kind.as_str(), &feature.shape, &feature.ellipse) {
            ("u-d-f", Some(MapShape::LineString(line)), None) => {
                Some(Self::path(Form::Line, line.clone()))
            }
            ("b-m-r", Some(MapShape::LineString(line)), None) => {
                Some(Self::path(Form::Route, line.clone()))
            }
            ("u-d-f", Some(MapShape::Polygon(rings)), None) => {
                Some(Self::path(Form::Polygon, ring(rings)?))
            }
            ("u-d-r", Some(MapShape::Polygon(rings)), None) => {
                Some(Self::path(Form::Rectangle, ring(rings)?)).filter(|it| it.points.len() == 4)
            }
            ("u-d-c-c", _, Some(ellipse)) if ellipse.major == ellipse.minor => Some(Self::circle(
                [feature.point.lon, feature.point.lat],
                ellipse.major,
            )),
            _ => None,
        }
    }

    /// Whether there is enough of it to publish.
    pub fn complete(&self) -> bool {
        match self.form {
            Form::Circle => self.points.len() == 1 && self.radius > 0.0,
            Form::Rectangle => self.points.len() == 4 && self.points[0] != self.points[2],
            form => self.points.len() >= form.least(),
        }
    }

    /// Where its marker goes: where a route starts, and the middle of
    /// anything else, which is where ATAK puts them.
    pub fn anchor(&self) -> [f64; 2] {
        let Some(first) = self.points.first() else {
            return [0.0, 0.0];
        };
        if matches!(self.form, Form::Route | Form::Circle) {
            return *first;
        }

        let bound = |axis: usize, pick: fn(f64, f64) -> f64| {
            self.points
                .iter()
                .map(|at| at[axis])
                .fold(first[axis], pick)
        };

        [
            (bound(0, f64::min) + bound(0, f64::max)) / 2.0,
            (bound(1, f64::min) + bound(1, f64::max)) / 2.0,
        ]
    }

    /// The outline as it is published. A circle has none: it is an
    /// [`ellipse`](Self::ellipse) around its anchor.
    pub fn shape(&self) -> Option<MapShape> {
        match self.form {
            Form::Circle => None,
            Form::Line | Form::Route => Some(MapShape::LineString(self.points.clone())),
            Form::Polygon | Form::Rectangle => Some(MapShape::Polygon(vec![self.traced()])),
        }
    }

    pub fn ellipse(&self) -> Option<MapEllipse> {
        (self.form == Form::Circle).then_some(MapEllipse {
            major: self.radius,
            minor: self.radius,
            angle: 0.0,
        })
    }

    /// The line a map draws for it: closed when it is an area, and a circle
    /// as the ring that approximates it.
    pub fn traced(&self) -> Vec<[f64; 2]> {
        let mut traced = match (self.form, self.points.first()) {
            (Form::Circle, Some(centre)) => ring(*centre, self.radius),
            _ => self.points.clone(),
        };
        // Fewer than three enclose nothing to close.
        if self.form.closed() && traced.len() >= 3 {
            traced.extend(traced.first().copied());
        }

        traced
    }

    /// How far it is along the line, or around the outline, in metres.
    pub fn length(&self) -> f64 {
        match self.form {
            Form::Circle => std::f64::consts::TAU * self.radius,
            _ => self
                .traced()
                .windows(2)
                .map(|pair| distance(pair[0], pair[1]))
                .sum(),
        }
    }

    /// Moves vertex `index` to `to`. A rectangle stays one: the corner
    /// opposite keeps its place and the two between are put back on the
    /// sides, whichever way the rectangle is turned.
    pub fn moved(&mut self, index: usize, to: [f64; 2]) {
        if index >= self.points.len() {
            return;
        }
        if self.form != Form::Rectangle || self.points.len() != 4 {
            self.points[index] = to;
            return;
        }

        let (next, opposite, previous) = ((index + 1) % 4, (index + 2) % 4, (index + 3) % 4);
        let fixed = self.points[opposite];
        // Flat metres around the fixed corner, which is as far as a rectangle
        // somebody drew by hand is from flat.
        let scale = [
            METRES_PER_DEGREE * fixed[1].to_radians().cos().abs().max(1e-6),
            METRES_PER_DEGREE,
        ];
        let flat = |at: [f64; 2]| [(at[0] - fixed[0]) * scale[0], (at[1] - fixed[1]) * scale[1]];
        let round = |at: [f64; 2]| [fixed[0] + at[0] / scale[0], fixed[1] + at[1] / scale[1]];

        let dragged = flat(to);
        // Along each side that meets the fixed corner, as it lies now.
        let onto = |side: [f64; 2]| {
            let length = side[0].hypot(side[1]);
            if length < 1e-9 {
                return [0.0, 0.0];
            }
            let unit = [side[0] / length, side[1] / length];
            let along = dragged[0] * unit[0] + dragged[1] * unit[1];
            [unit[0] * along, unit[1] * along]
        };

        self.points[next] = round(onto(flat(self.points[next])));
        self.points[previous] = round(onto(flat(self.points[previous])));
        self.points[index] = to;
    }
}

/// The great-circle distance between two positions, in metres.
pub fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (lat_a, lat_b) = (a[1].to_radians(), b[1].to_radians());
    let (d_lat, d_lon) = (lat_b - lat_a, (b[0] - a[0]).to_radians());
    let h = (d_lat / 2.0).sin().powi(2) + lat_a.cos() * lat_b.cos() * (d_lon / 2.0).sin().powi(2);

    2.0 * EARTH_RADIUS * h.sqrt().min(1.0).asin()
}

/// A circle as the open ring of positions that approximates it.
fn ring(centre: [f64; 2], radius: f64) -> Vec<[f64; 2]> {
    let metres_per_degree_lon = METRES_PER_DEGREE * centre[1].to_radians().cos().abs().max(1e-6);

    (0..CIRCLE_STEPS)
        .map(|step| {
            let around = std::f64::consts::TAU * step as f64 / CIRCLE_STEPS as f64;

            [
                centre[0] + radius * around.sin() / metres_per_degree_lon,
                centre[1] + radius * around.cos() / METRES_PER_DEGREE,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::render::tests::feature;
    use super::*;

    const A: [f64; 2] = [-0.12, 51.50];
    const B: [f64; 2] = [-0.11, 51.50];
    const C: [f64; 2] = [-0.11, 51.51];
    const D: [f64; 2] = [-0.12, 51.51];

    fn drawn(kind: &str, shape: Option<MapShape>, ellipse: Option<MapEllipse>) -> MapFeature {
        MapFeature {
            shape,
            ellipse,
            ..feature(kind)
        }
    }

    #[test]
    fn what_is_published_reads_back_as_what_was_drawn() {
        for geometry in [
            Geometry::path(Form::Line, vec![A, B, C]),
            Geometry::path(Form::Route, vec![A, B, C]),
            Geometry::path(Form::Polygon, vec![A, B, C]),
            Geometry::rectangle(A, C),
            Geometry::circle([-0.12, 51.5], 250.0),
        ] {
            assert!(geometry.complete(), "{:?}", geometry.form);

            let mut published = drawn(geometry.form.kind(), geometry.shape(), geometry.ellipse());
            [published.point.lon, published.point.lat] = geometry.anchor();

            assert_eq!(Geometry::of(&published), Some(geometry));
        }
    }

    #[test]
    fn an_area_is_published_closed_and_a_line_is_not() {
        let Some(MapShape::Polygon(rings)) = Geometry::rectangle(A, C).shape() else {
            panic!("a rectangle is an area");
        };
        assert_eq!(rings, [vec![A, B, C, D, A]]);

        assert_eq!(
            Geometry::path(Form::Route, vec![A, B]).shape(),
            Some(MapShape::LineString(vec![A, B]))
        );
    }

    #[test]
    fn somebody_elses_kind_of_drawing_is_not_taken_apart() {
        let line = Some(MapShape::LineString(vec![A, B]));
        let triangle = Some(MapShape::Polygon(vec![vec![A, B, C, A]]));
        let oval = Some(MapEllipse {
            major: 200.0,
            minor: 100.0,
            angle: 30.0,
        });

        for (why, feature) in [
            ("a marker", drawn("a-u-G", None, None)),
            ("a telestration", drawn("u-d-f-m", line, None)),
            (
                "a rectangle of three corners",
                drawn("u-d-r", triangle, None),
            ),
            ("an ellipse", drawn("u-d-c-c", None, oval)),
        ] {
            assert_eq!(Geometry::of(&feature), None, "{why}");
        }
    }

    #[test]
    fn a_drawing_is_anchored_in_its_middle_and_a_route_at_its_start() {
        let middle = Geometry::rectangle(A, C).anchor();
        assert!((middle[0] + 0.115).abs() < 1e-9 && (middle[1] - 51.505).abs() < 1e-9);

        assert_eq!(Geometry::path(Form::Route, vec![B, C]).anchor(), B);
    }

    #[test]
    fn lengths_are_metres_over_the_ground() {
        // A hundredth of a degree of latitude is a little over a kilometre.
        let north = Geometry::path(Form::Line, vec![A, D]).length();
        assert!((north - 1_112.0).abs() < 5.0, "{north}");

        let around = Geometry::circle(A, 100.0).length();
        assert!((around - 628.3).abs() < 0.1, "{around}");
    }

    #[test]
    fn dragging_a_corner_of_a_rectangle_leaves_a_rectangle() {
        let mut rectangle = Geometry::rectangle(A, C);
        rectangle.moved(2, [-0.10, 51.52]);

        assert_eq!(rectangle.points[0], A, "the opposite corner stays");
        assert_eq!(rectangle.points[2], [-0.10, 51.52]);
        let close =
            |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9;
        assert!(
            close(rectangle.points[1], [-0.10, 51.50]),
            "{:?}",
            rectangle.points
        );
        assert!(
            close(rectangle.points[3], [-0.12, 51.52]),
            "{:?}",
            rectangle.points
        );

        // Anything else moves the one vertex and no other.
        let mut line = Geometry::path(Form::Line, vec![A, B, C]);
        line.moved(1, D);
        line.moved(9, D);
        assert_eq!(line.points, [A, D, C]);
    }
}
