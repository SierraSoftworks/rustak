//! A marker as somebody is editing it: text in fields, before it is a feature.
//!
//! Every field is a string, because that is what an input holds, and a value
//! half-typed is still a value the form has to show. [`Draft::publish`] is
//! where the strings become numbers and codes, and where anything that cannot
//! is named — one complaint, the first, in the words somebody can act on.
//! Nothing here touches the browser, so all of it is tested natively.

use rustak_api::{MapFeature, MapPoint, PublishFeature};

/// The types worth offering somebody placing a marker, and what to call them.
/// A suggestion, not a constraint: any well-formed type is accepted.
pub const TYPES: &[(&str, &str)] = &[
    ("b-m-p-s-m", "Spot marker"),
    ("b-m-p-w", "Waypoint"),
    ("a-u-G", "Unknown · ground"),
    ("a-u-A", "Unknown · air"),
    ("a-u-S", "Unknown · sea surface"),
    ("a-f-G", "Friendly · ground"),
    ("a-f-G-U-C", "Friendly · ground unit"),
    ("a-f-G-E-V", "Friendly · vehicle"),
    ("a-f-A", "Friendly · air"),
    ("a-f-S", "Friendly · sea surface"),
    ("a-n-G", "Neutral · ground"),
    ("a-n-S", "Neutral · sea surface"),
    ("a-h-G", "Hostile · ground"),
    ("a-h-G-E-V", "Hostile · vehicle"),
    ("a-h-A", "Hostile · air"),
    ("a-h-S", "Hostile · sea surface"),
    ("b-a-o-tbl", "Alert · troops in contact"),
    ("b-a-o-can", "Alert · cancel"),
];

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
}

impl Draft {
    /// The fields a feature would fill in.
    pub fn of(feature: &MapFeature) -> Self {
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
            how: None,
            remarks: (!remarks.is_empty()).then(|| remarks.to_string()),
            sidc,
            stale: None,
            groups: self.groups.clone(),
        })
    }
}

/// Whether a feature is something the console may edit: a point a person
/// placed or typed. Not what a device measured, which would only be
/// overwritten by the device's next report; and not a route or a drawing,
/// whose outline this form does not carry and a save would throw away.
pub fn editable(feature: &MapFeature) -> bool {
    feature.shape.is_none()
        && feature
            .how
            .as_deref()
            .is_none_or(|how| how.starts_with("h-"))
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
fn well_formed_type(kind: &str) -> bool {
    let mut segments = kind.split('-');

    matches!(segments.next(), Some(first) if first.len() == 1 && first.chars().all(|c| c.is_ascii_lowercase()))
        && segments.all(|segment| {
            !segment.is_empty() && segment.chars().all(|c| c.is_ascii_alphanumeric())
        })
        && kind.len() <= 64
}

#[cfg(test)]
mod tests {
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
        drawn.shape = Some(rustak_api::MapShape::LineString(Vec::new()));
        assert!(!editable(&drawn), "an outline this form would throw away");
    }
}
