//! A drawing in progress, and one being reshaped.
//!
//! A [`Sketch`] is what the clicks so far add up to. A line, a polygon and a
//! route take a vertex per click and are finished by clicking the last vertex
//! again — which is what a double click does — or, for a polygon, the first;
//! a rectangle is a corner and its opposite, a circle its centre and its
//! edge. An [`Outline`] is a drawing that exists, with a handle on each
//! vertex to drag.
//!
//! Both are handed to `js/map.js` as the same GeoJSON: the line so far, the
//! area it would enclose, and the handles. The glue knows which handle a
//! click or a drag landed on and nothing about what that means.

use serde::Deserialize;
use serde_json::{Value, json};

use super::geometry::{Form, Geometry, distance};

/// Which end of a sketch a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Near {
    First,
    Last,
}

/// What the pointer did over a sketch or an outline, as `js/map.js` says it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pointer {
    /// It is over `[lon, lat]`, while something is being drawn.
    Hover([f64; 2]),
    /// It has dragged handle `index` to `at`, and has or has not let go.
    Drag {
        index: usize,
        at: [f64; 2],
        done: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sketch {
    form: Form,
    points: Vec<[f64; 2]>,
    /// Where the pointer is, which is where the next vertex would go.
    hover: Option<[f64; 2]>,
}

impl Sketch {
    pub fn new(form: Form) -> Self {
        Self {
            form,
            points: Vec::new(),
            hover: None,
        }
    }

    /// Takes a click at `at`. Answers the drawing once that click finishes it.
    pub fn click(&mut self, at: [f64; 2], near: Option<Near>) -> Option<Geometry> {
        self.hover = None;

        if self.form.two_clicks() {
            // The same place twice is a double click, not a shape.
            if self.points.first() != Some(&at) {
                self.points.push(at);
            }
            return self.finish();
        }

        let closes = near == Some(Near::Last) || (near.is_some() && self.form.closed());
        match (closes, near) {
            (true, _) => return self.finish(),
            // A click on a vertex that finishes nothing adds nothing either.
            (false, Some(_)) => {}
            (false, None) => self.points.push(at),
        }

        None
    }

    pub fn hover(&mut self, at: [f64; 2]) {
        self.hover = Some(at);
    }

    /// Takes back the last click.
    pub fn undo(&mut self) {
        self.points.pop();
    }

    pub fn can_undo(&self) -> bool {
        !self.points.is_empty()
    }

    /// The drawing as it stands, when there is enough of it to be one.
    pub fn finish(&self) -> Option<Geometry> {
        self.geometry(&self.points).filter(Geometry::complete)
    }

    /// What to do next, in words.
    pub fn hint(&self) -> &'static str {
        match (self.form, self.points.len()) {
            (Form::Rectangle, 0) => "Click one corner of the rectangle.",
            (Form::Rectangle, _) => "Click the opposite corner.",
            (Form::Circle, 0) => "Click the centre of the circle.",
            (Form::Circle, _) => "Click where its edge should be.",
            (_, 0) => "Click the map to start drawing.",
            (form, count) if count < form.least() => "Click to add the next point.",
            (Form::Polygon, _) => "Click to add a point, or the first or last point to finish.",
            _ => "Click to add a point, or the last point again to finish.",
        }
    }

    /// The sketch for the map: see [`overlay`]. The pointer stands in for the
    /// vertex the next click would add.
    pub fn draw(&self) -> Value {
        let mut ahead = self.points.clone();
        ahead.extend(self.hover);

        let traced = self
            .geometry(&ahead)
            .map(|geometry| geometry.traced())
            .unwrap_or_default();

        overlay(&traced, self.form.closed(), &self.points)
    }

    fn geometry(&self, points: &[[f64; 2]]) -> Option<Geometry> {
        match (self.form, points) {
            (Form::Rectangle, [a, c]) => Some(Geometry::rectangle(*a, *c)),
            (Form::Circle, [centre, edge]) => {
                Some(Geometry::circle(*centre, distance(*centre, *edge)))
            }
            (Form::Rectangle | Form::Circle, _) => None,
            (form, points) => Some(Geometry::path(form, points.to_vec())),
        }
    }
}

/// A drawing that exists, as it is being reshaped.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline {
    pub uid: String,
    pub geometry: Geometry,
    /// Whether a handle has been dragged since it was last saved.
    pub moved: bool,
}

impl Outline {
    /// Something to reshape, when `geometry` is reshaped by dragging: a
    /// circle is its centre and radius, which are typed.
    pub fn of(uid: &str, geometry: Geometry) -> Option<Self> {
        (geometry.form != Form::Circle).then(|| Self {
            uid: uid.to_string(),
            geometry,
            moved: false,
        })
    }

    pub fn drag(&mut self, index: usize, to: [f64; 2]) {
        self.geometry.moved(index, to);
        self.moved = true;
    }

    pub fn draw(&self) -> Value {
        overlay(
            &self.geometry.traced(),
            self.geometry.form.closed(),
            &self.geometry.points,
        )
    }
}

/// What `js/map.js` draws over the map: the line, the area inside it when it
/// is one, and a handle on each vertex that says where in the run it is.
fn overlay(traced: &[[f64; 2]], closed: bool, handles: &[[f64; 2]]) -> Value {
    let mut features = Vec::new();

    if traced.len() >= 2 {
        features.push(json!({
            "type": "Feature",
            "geometry": { "type": "LineString", "coordinates": traced },
            "properties": { "part": "line" },
        }));
    }
    if closed && traced.len() >= 4 {
        features.push(json!({
            "type": "Feature",
            "geometry": { "type": "Polygon", "coordinates": [traced] },
            "properties": { "part": "fill" },
        }));
    }

    let last = handles.len().saturating_sub(1);
    features.extend(handles.iter().enumerate().map(|(index, at)| {
        let role = match index {
            _ if index == last => "last",
            0 => "first",
            _ => "between",
        };

        json!({
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": at },
            "properties": { "part": "handle", "index": index, "role": role },
        })
    }));

    json!({ "type": "FeatureCollection", "features": features })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [f64; 2] = [-0.12, 51.50];
    const B: [f64; 2] = [-0.11, 51.50];
    const C: [f64; 2] = [-0.11, 51.51];

    fn parts(drawn: &Value) -> Vec<&str> {
        drawn["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|feature| feature["properties"]["part"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn a_path_takes_a_vertex_a_click_until_its_end_is_clicked_again() {
        for (form, closing, vertices) in [
            (Form::Line, Near::Last, 2),
            (Form::Route, Near::Last, 2),
            (Form::Polygon, Near::First, 3),
            (Form::Polygon, Near::Last, 3),
        ] {
            let mut sketch = Sketch::new(form);
            for at in [A, B, C].into_iter().take(vertices) {
                assert_eq!(sketch.click(at, None), None, "{form:?}");
            }

            let drawn = sketch.click(C, Some(closing)).expect("that finishes it");
            assert_eq!(drawn.form, form);
            assert_eq!(
                drawn.points.len(),
                vertices,
                "the closing click adds nothing"
            );
        }
    }

    #[test]
    fn an_end_clicked_too_soon_finishes_nothing_and_adds_nothing() {
        let mut sketch = Sketch::new(Form::Polygon);
        sketch.click(A, None);
        sketch.click(B, None);

        assert_eq!(sketch.click(B, Some(Near::Last)), None);
        assert_eq!(sketch.finish(), None, "two points enclose nothing");

        // The start of a line is not its end.
        let mut line = Sketch::new(Form::Line);
        line.click(A, None);
        line.click(B, None);
        assert_eq!(line.click(A, Some(Near::First)), None);
        assert_eq!(line.finish().map(|drawn| drawn.points), Some(vec![A, B]));
    }

    #[test]
    fn a_rectangle_and_a_circle_are_two_clicks() {
        let mut rectangle = Sketch::new(Form::Rectangle);
        assert_eq!(rectangle.click(A, None), None);
        assert_eq!(rectangle.click(A, Some(Near::Last)), None, "a double click");
        assert_eq!(rectangle.click(C, None), Some(Geometry::rectangle(A, C)));

        let mut circle = Sketch::new(Form::Circle);
        circle.click(A, None);
        let drawn = circle.click(B, None).expect("the second click is its edge");
        assert!((drawn.radius - distance(A, B)).abs() < 1e-9);
        assert_eq!(drawn.points, [A]);
    }

    #[test]
    fn a_click_can_be_taken_back() {
        let mut sketch = Sketch::new(Form::Line);
        assert!(!sketch.can_undo());

        sketch.click(A, None);
        sketch.click(B, None);
        sketch.undo();

        assert_eq!(sketch.finish(), None);
        assert!(sketch.can_undo());
    }

    #[test]
    fn the_pointer_stands_in_for_the_next_vertex() {
        let mut sketch = Sketch::new(Form::Polygon);
        sketch.click(A, None);
        assert_eq!(parts(&sketch.draw()), ["handle"]);

        sketch.click(B, None);
        sketch.hover(C);
        assert_eq!(parts(&sketch.draw()), ["line", "fill", "handle", "handle"]);

        let drawn = sketch.draw();
        let roles: Vec<_> = drawn["features"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|feature| feature["properties"]["role"].as_str())
            .collect();
        assert_eq!(roles, ["first", "last"]);
    }

    #[test]
    fn an_outline_is_reshaped_by_its_handles_and_a_circle_is_not() {
        let mut outline = Outline::of("SHAPE-1", Geometry::path(Form::Line, vec![A, B]))
            .expect("a line has handles");
        assert!(!outline.moved);

        outline.drag(1, C);
        assert!(outline.moved);
        assert_eq!(outline.geometry.points, [A, C]);
        assert_eq!(parts(&outline.draw()), ["line", "handle", "handle"]);

        assert_eq!(Outline::of("CIRCLE-1", Geometry::circle(A, 100.0)), None);
    }

    #[test]
    fn the_glue_says_what_the_pointer_did_in_json() {
        assert_eq!(
            serde_json::from_str::<Pointer>(r#"{"hover":[-0.12,51.5]}"#).unwrap(),
            Pointer::Hover(A)
        );
        assert_eq!(
            serde_json::from_str::<Pointer>(
                r#"{"drag":{"index":2,"at":[-0.11,51.5],"done":true}}"#
            )
            .unwrap(),
            Pointer::Drag {
                index: 2,
                at: B,
                done: true
            }
        );
    }
}
