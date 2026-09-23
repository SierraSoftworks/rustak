//! A marker or a drawing as somebody is editing it: text in fields, before it
//! is a feature.
//!
//! Every field is a string, because that is what an input holds, and a value
//! half-typed is still a value the form has to show. [`Draft::publish`] is
//! where the strings become numbers and codes, and where anything that cannot
//! is named — one complaint, the first, in the words somebody can act on.
//! A drawing's outline is not text: it is the [`Geometry`] that was drawn, and
//! the map is where it is changed. Nothing here touches the browser, so all of
//! it is tested natively.

use rustak_api::{MapFeature, MapPoint, MapStyle, PublishFeature};

use super::geometry::{Form, Geometry};

/// What the uid of something made here begins with, so that the next one can
/// be numbered after the ones before it — and so that a route made here can
/// be told from one whose waypoints somebody named on a device.
pub const PLACED_PREFIX: &str = "rustak-console-";

/// The colours a drawing is offered in: the ones ATAK's own picker leads
/// with, less the white that a light base map would lose.
pub const COLORS: &[(&str, &str)] = &[
    ("#ff0000", "Red"),
    ("#ff7700", "Orange"),
    ("#ffff00", "Yellow"),
    ("#00ff00", "Green"),
    ("#00ffff", "Cyan"),
    ("#0000ff", "Blue"),
    ("#ff00ff", "Magenta"),
    ("#000000", "Black"),
];

/// What a drawing is drawn in until somebody says otherwise.
pub const DRAWN_COLOR: &str = "#ff0000";

/// The line ATAK draws a new shape with.
const DRAWN_WEIGHT: f64 = 4.0;

/// How solid a new area is filled: enough to say where it is, and little
/// enough to see what is in it. ATAK's own default is more than twice this,
/// which on a device is a slider away and here would hide the map.
const DRAWN_FILL_OPACITY: f64 = 0.25;

/// The type a marker is placed as, until somebody says otherwise.
pub const PLACED_TYPE: &str = "b-m-p-s-m";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub uid: String,
    pub kind: String,
    pub callsign: String,
    pub sidc: String,
    pub lat: String,
    pub lon: String,
    /// Metres above the ellipsoid, or empty for unknown.
    pub hae: String,
    pub remarks: String,
    /// The channels it is published into, by name.
    pub groups: Vec<String>,

    /// The outline, for a drawing: what was drawn, or what the map has since
    /// made of it. [`None`] for a marker.
    pub geometry: Option<Geometry>,
    /// A circle's radius in metres, which is typed rather than dragged.
    pub radius: String,
    /// `#rrggbb`, for a drawing — or empty for one whose author never said,
    /// which is then left for whoever draws it to decide, as it was.
    pub color: String,
    /// What a drawing somebody else styled keeps through an edit here.
    weight: Option<f64>,
    fill_opacity: Option<f64>,
}

impl Draft {
    /// The fields a feature would fill in.
    pub fn of(feature: &MapFeature) -> Self {
        let style = feature.style.as_ref();

        Self {
            uid: feature.uid.clone(),
            kind: feature.kind.clone(),
            callsign: feature.callsign.clone().unwrap_or_default(),
            sidc: feature.sidc.clone().unwrap_or_default(),
            lat: format!("{:.6}", feature.point.lat),
            lon: format!("{:.6}", feature.point.lon),
            hae: feature.point.hae.map(metres).unwrap_or_default(),
            remarks: feature.remarks.clone().unwrap_or_default(),
            groups: feature.groups.clone(),
            radius: feature
                .ellipse
                .map(|ellipse| metres(ellipse.major))
                .unwrap_or_default(),
            color: style
                .and_then(|style| style.stroke.clone())
                .unwrap_or_default(),
            weight: style.and_then(|style| style.weight),
            fill_opacity: style.and_then(|style| style.fill_opacity),
            geometry: Geometry::of(feature),
        }
    }

    /// The draft with only the channels in `publishable` kept. What a feature
    /// was recorded under is every channel its sender held, and naming one the
    /// editor may not publish into would refuse the save over a channel the
    /// form never showed.
    pub fn within(mut self, publishable: &[String]) -> Self {
        self.groups.retain(|group| publishable.contains(group));
        self
    }

    /// A marker just placed at `[lon, lat]`, the `n`th on this map, into
    /// `groups`.
    pub fn placed(uid: impl Into<String>, at: [f64; 2], n: usize, groups: Vec<String>) -> Self {
        Self {
            uid: uid.into(),
            kind: PLACED_TYPE.to_string(),
            callsign: format!("Marker {n}"),
            lat: format!("{:.6}", at[1]),
            lon: format!("{:.6}", at[0]),
            groups,
            ..Self::default()
        }
    }

    /// Something just drawn, the `n`th thing made on this map, into `groups`.
    pub fn drawn(
        uid: impl Into<String>,
        geometry: Geometry,
        n: usize,
        groups: Vec<String>,
    ) -> Self {
        let [lon, lat] = geometry.anchor();

        Self {
            uid: uid.into(),
            kind: geometry.form.kind().to_string(),
            callsign: format!("{} {n}", geometry.form.label()),
            lat: format!("{lat:.6}"),
            lon: format!("{lon:.6}"),
            radius: metres(geometry.radius),
            color: DRAWN_COLOR.to_string(),
            geometry: Some(geometry),
            groups,
            ..Self::default()
        }
    }

    /// The outline as it stands: a circle's from the fields that say where it
    /// is and how big, and anything else's as it was drawn or dragged.
    fn outlined(&self, lat: f64, lon: f64) -> Result<Option<Geometry>, String> {
        let Some(geometry) = &self.geometry else {
            return Ok(None);
        };
        if geometry.form != Form::Circle {
            return Ok(Some(geometry.clone()));
        }

        number(&self.radius, "radius")?
            .filter(|radius| *radius > 0.0)
            .map(|radius| Some(Geometry::circle([lon, lat], radius)))
            .ok_or_else(|| "A circle's radius is in metres, and more than none.".to_string())
    }

    /// What to publish, or the first thing wrong with it.
    pub fn publish(&self) -> Result<PublishFeature, String> {
        let callsign = self.callsign.trim();
        if callsign.is_empty() {
            return Err("Give it a name.".to_string());
        }

        let kind = self.kind.trim();
        if !well_formed_type(kind) {
            return Err("The type should look like a-u-G or b-m-p-s-m.".to_string());
        }

        let lat = number(&self.lat, "latitude")?
            .filter(|lat| (-90.0..=90.0).contains(lat))
            .ok_or_else(|| "Latitude is between -90 and 90.".to_string())?;
        let lon = number(&self.lon, "longitude")?
            .filter(|lon| (-180.0..=180.0).contains(lon))
            .ok_or_else(|| "Longitude is between -180 and 180.".to_string())?;
        let hae = number(&self.hae, "altitude")?;

        let sidc = self.sidc.trim();
        let sidc =
            match sidc.is_empty() {
                true => None,
                false => Some(rustak_api::map::sidc(sidc).ok_or_else(|| {
                    "A symbol code is 15 letters, or 20 or 30 digits.".to_string()
                })?),
            };

        let remarks = self.remarks.trim();

        // A drawing is where its outline puts it, whatever the fields say.
        let geometry = self.outlined(lat, lon)?;
        let [lon, lat] = geometry.as_ref().map_or([lon, lat], Geometry::anchor);
        let style = geometry.as_ref().map(|geometry| {
            let color = Some(self.color.clone()).filter(|color| !color.is_empty());
            let filled = geometry.form.closed() && color.is_some();

            MapStyle {
                stroke: color.clone(),
                weight: Some(self.weight.unwrap_or(DRAWN_WEIGHT)),
                fill: color.filter(|_| filled),
                fill_opacity: filled.then(|| self.fill_opacity.unwrap_or(DRAWN_FILL_OPACITY)),
            }
        });

        Ok(PublishFeature {
            kind: kind.to_string(),
            callsign: callsign.to_string(),
            point: MapPoint {
                lat,
                lon,
                hae,
                ce: None,
                le: None,
            },
            // Drawn by hand, as ATAK says of a shape; a marker is left to the
            // server, which says it was placed on a map.
            how: geometry.as_ref().map(|_| "h-e".to_string()),
            remarks: (!remarks.is_empty()).then(|| remarks.to_string()),
            sidc,
            stale: None,
            groups: self.groups.clone(),
            shape: geometry.as_ref().and_then(Geometry::shape),
            ellipse: geometry.as_ref().and_then(Geometry::ellipse),
            style,
        })
    }
}

/// Whether a feature is something the console may edit: what a person
/// placed, typed or drew, and all of which this form carries.
///
/// Not what a device measured, which would only be overwritten by the
/// device's next report. Not a drawing of a kind the console does not make,
/// or one whose outline did not arrive or could not be read: a save would
/// throw the outline away, or publish a drawing's type with none. And not a route made elsewhere: a
/// device's route names its checkpoints and carries its navigation cues, and
/// a save from here would write it again without them.
pub fn editable(feature: &MapFeature) -> bool {
    let by_hand = feature
        .how
        .as_deref()
        .is_none_or(|how| how.starts_with("h-"));
    let carried = match Geometry::of(feature) {
        Some(geometry) => geometry.form != Form::Route || feature.uid.starts_with(PLACED_PREFIX),
        None => feature.shape.is_none() && !drawing_kind(&feature.kind),
    };

    by_hand && carried
}

/// Whether a CoT type is a drawing's, a route's or a range and bearing
/// line's: something that is an outline, whether or not one was read.
fn drawing_kind(kind: &str) -> bool {
    ["u-d", "u-r", "b-m-r"]
        .iter()
        .any(|family| kind == *family || kind.starts_with(&format!("{family}-")))
}

/// Metres as somebody would type them: to the centimetre, without the zeros
/// nobody would.
fn metres(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A number from a field, or [`None`] from an empty one.
fn number(text: &str, what: &str) -> Result<Option<f64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(Some)
        .ok_or_else(|| format!("The {what} is not a number."))
}

/// Whether text is shaped like a CoT type: dash-separated alphanumeric
/// segments, the first one lower-case letter.
pub(super) fn well_formed_type(kind: &str) -> bool {
    let mut segments = kind.split('-');

    matches!(segments.next(), Some(first) if first.len() == 1 && first.chars().all(|c| c.is_ascii_lowercase()))
        && segments.all(|segment| {
            !segment.is_empty() && segment.chars().all(|c| c.is_ascii_alphanumeric())
        })
        && kind.len() <= 64
}

#[cfg(test)]
mod tests {
    use rustak_api::MapShape;

    use super::*;

    fn draft() -> Draft {
        Draft {
            uid: "MARKER-1".to_string(),
            kind: "a-u-G".to_string(),
            callsign: " CONTACT 1 ".to_string(),
            sidc: String::new(),
            lat: "51.5".to_string(),
            lon: "-0.12".to_string(),
            hae: "".to_string(),
            remarks: " Two vehicles. ".to_string(),
            groups: vec!["Blue".to_string()],
            ..Draft::default()
        }
    }

    #[test]
    fn a_draft_publishes_as_what_was_typed_trimmed_and_parsed() {
        let published = draft().publish().unwrap();

        assert_eq!(published.callsign, "CONTACT 1");
        assert_eq!((published.point.lat, published.point.lon), (51.5, -0.12));
        assert_eq!(published.point.hae, None);
        assert_eq!(published.remarks.as_deref(), Some("Two vehicles."));
        assert_eq!(published.sidc, None);
        assert_eq!(published.groups, ["Blue"]);
    }

    #[test]
    fn the_first_thing_wrong_is_named() {
        let wrong = |change: fn(&mut Draft)| {
            let mut draft = draft();
            change(&mut draft);
            draft.publish().unwrap_err()
        };

        assert_eq!(wrong(|d| d.callsign = "  ".into()), "Give it a name.");
        assert!(wrong(|d| d.kind = "A-u-G".into()).contains("type"));
        assert!(wrong(|d| d.lat = "91".into()).contains("Latitude"));
        assert!(wrong(|d| d.lon = "east".into()).contains("longitude"));
        assert!(wrong(|d| d.hae = "high".into()).contains("altitude"));
        assert!(wrong(|d| d.sidc = "SFG".into()).contains("symbol code"));
        assert!(
            draft().publish().is_ok(),
            "and a draft with nothing wrong publishes"
        );
    }

    #[test]
    fn a_channel_the_editor_may_not_publish_into_is_not_sent() {
        let mut recorded = draft();
        recorded.groups = vec!["Blue".to_string(), "Red".to_string()];

        assert_eq!(recorded.within(&["Blue".to_string()]).groups, ["Blue"]);
    }

    #[test]
    fn a_placed_marker_is_a_spot_marker_where_the_click_was() {
        let placed = Draft::placed("M-1", [-0.12, 51.5], 3, vec!["Blue".to_string()]);

        assert_eq!(placed.kind, PLACED_TYPE);
        assert_eq!(placed.callsign, "Marker 3");
        assert_eq!(placed.publish().unwrap().point.lat, 51.5);
    }

    #[test]
    fn what_a_person_placed_may_be_edited_and_what_a_device_measured_may_not() {
        let feature = |how: Option<&str>| MapFeature {
            uid: "X".to_string(),
            kind: "a-u-G".to_string(),
            how: how.map(str::to_owned),
            callsign: None,
            team: None,
            role: None,
            time: "2026-09-18T12:00:00Z".parse().unwrap(),
            stale: "2026-09-18T12:00:00Z".parse().unwrap(),
            received_at: "2026-09-18T12:00:00Z".parse().unwrap(),
            point: MapPoint {
                lat: 0.0,
                lon: 0.0,
                hae: Some(12.4),
                ce: None,
                le: None,
            },
            shape: None,
            ellipse: None,
            style: None,
            course: None,
            speed: None,
            battery: None,
            remarks: None,
            software: None,
            sidc: None,
            groups: Vec::new(),
        };

        assert!(editable(&feature(Some("h-g-i-g-o"))));
        assert!(editable(&feature(None)));
        assert!(!editable(&feature(Some("m-g"))));
        assert_eq!(
            Draft::of(&feature(None)).hae,
            "12.4",
            "to the precision it had"
        );

        let mut drawn = feature(Some("h-e"));
        drawn.shape = Some(MapShape::LineString(Vec::new()));
        assert!(!editable(&drawn), "an outline this form would throw away");

        // A drawing whose outline never arrived is still not a marker.
        for kind in ["u-d-r", "u-d-c-c", "u-d-f-m", "b-m-r", "u-r-b-c-c"] {
            let unread = MapFeature {
                kind: kind.to_string(),
                ..feature(Some("h-e"))
            };
            assert!(!editable(&unread), "{kind} with no outline");
        }

        // A drawing of a kind made here is carried whole, wherever it was
        // made — except a route, which a device says more about than this.
        let line = Some(MapShape::LineString(vec![[-0.12, 51.5], [-0.11, 51.5]]));
        for (uid, kind, expected) in [
            ("ATAK-1", "u-d-f", true),
            ("ATAK-1", "b-m-r", false),
            ("rustak-console-1", "b-m-r", true),
            ("ATAK-1", "u-d-f-m", false),
        ] {
            let drawing = MapFeature {
                uid: uid.to_string(),
                kind: kind.to_string(),
                shape: line.clone(),
                ..feature(Some("h-e"))
            };
            assert_eq!(editable(&drawing), expected, "{kind} from {uid}");
        }
    }

    #[test]
    fn a_drawing_publishes_its_outline_where_the_outline_is_and_in_its_colour() {
        let corners = [[-0.12, 51.50], [-0.10, 51.50], [-0.10, 51.52]];
        let drawn = Draft::drawn(
            "rustak-console-1",
            Geometry::path(Form::Polygon, corners.to_vec()),
            2,
            Vec::new(),
        );
        assert_eq!(drawn.callsign, "Polygon 2");

        let published = drawn.publish().unwrap();
        assert_eq!(published.kind, "u-d-f");
        assert_eq!(published.how.as_deref(), Some("h-e"));
        assert!(
            (published.point.lat - 51.51).abs() < 1e-9 && (published.point.lon + 0.11).abs() < 1e-9,
            "anchored in its middle: {:?}",
            published.point
        );
        let Some(MapShape::Polygon(rings)) = &published.shape else {
            panic!("a polygon is published as one");
        };
        assert_eq!(rings[0].first(), rings[0].last());

        let style = published.style.expect("a drawing says how it is drawn");
        assert_eq!(style.stroke.as_deref(), Some(DRAWN_COLOR));
        assert_eq!(style.fill.as_deref(), Some(DRAWN_COLOR));

        // A line encloses nothing to fill, and a marker has no style at all.
        let line = Draft::drawn(
            "L",
            Geometry::path(Form::Line, corners.to_vec()),
            1,
            Vec::new(),
        );
        assert_eq!(line.publish().unwrap().style.unwrap().fill, None);
        assert_eq!(draft().publish().unwrap().style, None);
    }

    #[test]
    fn a_circle_is_where_and_as_wide_as_its_fields_say() {
        let mut circle = Draft::drawn("C", Geometry::circle([-0.12, 51.5], 250.0), 1, Vec::new());
        assert_eq!(circle.radius, "250");

        circle.radius = "400".to_string();
        circle.lat = "51.6".to_string();
        let published = circle.publish().unwrap();
        assert_eq!(published.ellipse.map(|ellipse| ellipse.major), Some(400.0));
        assert_eq!(published.point.lat, 51.6);
        assert_eq!(published.shape, None);

        for radius in ["", "0", "-5", "wide"] {
            circle.radius = radius.to_string();
            assert!(circle.publish().is_err(), "a radius of {radius:?}");
        }
    }
}
