//! Where one thing has been: its fixes in order, and the line through them.
//!
//! A track is the history read back for whatever the pop-over is open on — the
//! same features the map draws live, oldest first. It answers the two
//! questions the page asks of it: which fix was current at a moment, and what
//! the line looks like with that moment on it. Nothing here touches the
//! browser, so all of it is tested natively.

use chrono::{DateTime, Utc};
use rustak_api::MapFeature;
use serde_json::{Value, json};

pub struct Track {
    uid: String,
    /// Oldest first, by the event's own time, one per moment.
    fixes: Vec<MapFeature>,
}

impl Track {
    /// A track through `fixes`, in whatever order they came. Anything that is
    /// not `uid`'s is left out.
    pub fn new(uid: impl Into<String>, mut fixes: Vec<MapFeature>) -> Self {
        let uid = uid.into();

        fixes.retain(|fix| fix.uid == uid);
        fixes.sort_by_key(|fix| fix.time);
        fixes.dedup_by_key(|fix| fix.time);

        Self { uid, fixes }
    }

    pub fn uid(&self) -> &str {
        &self.uid
    }

    pub fn len(&self) -> usize {
        self.fixes.len()
    }

    /// What the thing is called, from the last thing it said.
    pub fn name(&self) -> String {
        self.fixes
            .last()
            .and_then(|fix| fix.callsign.clone())
            .unwrap_or_else(|| self.uid.clone())
    }

    /// From the first fix to the last.
    pub fn span(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        Some((self.fixes.first()?.time, self.fixes.last()?.time))
    }

    /// Takes a fix off the live feed: kept when it is this track's and newer
    /// than anything it has. Answers whether it was.
    pub fn extend(&mut self, feature: &MapFeature) -> bool {
        let newer = feature.uid == self.uid
            && self
                .fixes
                .last()
                .is_none_or(|last| feature.time > last.time);

        if newer {
            self.fixes.push(feature.clone());
        }

        newer
    }

    /// The fix that was current at `when`: the last one at or before it, or
    /// the first when `when` is before all of them.
    pub fn at(&self, when: DateTime<Utc>) -> Option<&MapFeature> {
        let after = self.fixes.partition_point(|fix| fix.time <= when);

        self.fixes.get(after.saturating_sub(1))
    }

    /// The line, as `js/map.js` draws it: the part travelled by `until`, the
    /// part still to come, and every fix along it. With no `until`, all of it
    /// has been travelled.
    pub fn draw(&self, until: Option<DateTime<Utc>>, color: &str) -> Value {
        let position = |fix: &MapFeature| json!([fix.point.lon, fix.point.lat]);
        let line = |part: &str, fixes: &[MapFeature]| {
            (fixes.len() >= 2).then(|| {
                json!({
                    "type": "Feature",
                    "geometry": {
                        "type": "LineString",
                        "coordinates": fixes.iter().map(position).collect::<Vec<_>>(),
                    },
                    "properties": { "part": part, "color": color },
                })
            })
        };

        // The moment shown belongs to both parts, so the line is unbroken.
        let travelled = match until {
            Some(until) => self.fixes.partition_point(|fix| fix.time <= until),
            None => self.fixes.len(),
        };
        let split = travelled.max(1).min(self.fixes.len());

        let mut features: Vec<Value> = Vec::new();
        features.extend(line("past", &self.fixes[..split]));
        features.extend(line("future", &self.fixes[split.saturating_sub(1)..]));
        features.push(json!({
            "type": "Feature",
            "geometry": {
                "type": "MultiPoint",
                "coordinates": self.fixes.iter().map(position).collect::<Vec<_>>(),
            },
            "properties": { "part": "fix", "color": color },
        }));

        json!({ "type": "FeatureCollection", "features": features })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use rustak_api::MapPoint;

    use super::*;

    fn at(time: &str) -> DateTime<Utc> {
        format!("2026-09-18T{time}Z").parse().unwrap()
    }

    fn fix(uid: &str, time: &str, lon: f64) -> MapFeature {
        MapFeature {
            uid: uid.to_string(),
            kind: "a-f-G-U-C".to_string(),
            how: None,
            callsign: Some("QUINN".to_string()),
            team: None,
            role: None,
            time: at(time),
            stale: at(time) + Duration::minutes(2),
            received_at: at(time),
            point: MapPoint {
                lat: 51.5,
                lon,
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

    fn track() -> Track {
        Track::new(
            "A",
            vec![
                fix("A", "12:02:00", -0.12),
                fix("A", "12:00:00", -0.10),
                fix("B", "12:01:00", -0.50),
                fix("A", "12:01:00", -0.11),
                fix("A", "12:01:00", -0.11),
            ],
        )
    }

    #[test]
    fn a_track_is_one_things_fixes_in_order_of_time() {
        let track = track();

        assert_eq!(track.len(), 3);
        assert_eq!(track.span(), Some((at("12:00:00"), at("12:02:00"))));
        assert_eq!(track.name(), "QUINN");
    }

    #[test]
    fn the_fix_at_a_moment_is_the_last_one_before_it() {
        let track = track();
        let lon = |when: &str| track.at(at(when)).map(|fix| fix.point.lon);

        assert_eq!(lon("11:00:00"), Some(-0.10));
        assert_eq!(lon("12:00:00"), Some(-0.10));
        assert_eq!(lon("12:01:30"), Some(-0.11));
        assert_eq!(lon("13:00:00"), Some(-0.12));
        assert_eq!(Track::new("A", Vec::new()).at(at("12:00:00")), None);
    }

    #[test]
    fn the_feed_extends_a_track_only_forwards_and_only_with_its_own() {
        let mut track = track();

        assert!(!track.extend(&fix("B", "12:03:00", -0.5)));
        assert!(!track.extend(&fix("A", "12:01:30", -0.5)));
        assert!(track.extend(&fix("A", "12:03:00", -0.13)));
        assert_eq!(track.len(), 4);
    }

    #[test]
    fn the_line_is_split_at_the_moment_shown_without_a_gap() {
        let drawn = track().draw(Some(at("12:01:00")), "#123456");
        let parts: Vec<(&str, usize)> = drawn["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|feature| {
                (
                    feature["properties"]["part"].as_str().unwrap(),
                    feature["geometry"]["coordinates"].as_array().unwrap().len(),
                )
            })
            .collect();

        assert_eq!(parts, [("past", 2), ("future", 2), ("fix", 3)]);
        assert_eq!(drawn["features"][0]["properties"]["color"], "#123456");
        assert_eq!(
            drawn["features"][0]["geometry"]["coordinates"][1],
            json!([-0.11, 51.5])
        );
    }

    #[test]
    fn a_live_track_is_all_past_and_a_short_one_draws_no_line() {
        let parts = |drawn: Value| -> Vec<String> {
            drawn["features"]
                .as_array()
                .unwrap()
                .iter()
                .map(|feature| feature["properties"]["part"].as_str().unwrap().to_owned())
                .collect()
        };

        assert_eq!(parts(track().draw(None, "#000")), ["past", "fix"]);
        assert_eq!(
            parts(Track::new("A", vec![fix("A", "12:00:00", -0.1)]).draw(None, "#000")),
            ["fix"]
        );
    }
}
