//! What is said about the thing in focus: who, what, where, how fast, how
//! fresh — and then what only the server knows, which channels the sender was
//! publishing into, the first thing to check when somebody says they cannot
//! see it. Nothing here touches the browser, so all of it is tested natively.

use rustak_api::MapFeature;

use crate::util::mgrs;

/// The rows of the panel, in the order somebody reads them. A row the
/// message said nothing about is left out rather than shown empty.
pub fn facts(feature: &MapFeature) -> Vec<(&'static str, String)> {
    let point = &feature.point;
    let mut rows = Vec::new();

    if let Some(team) = &feature.team {
        rows.push((
            "Team",
            match &feature.role {
                Some(role) => format!("{team} · {role}"),
                None => team.clone(),
            },
        ));
    }

    rows.push(("Position", format!("{:.5}, {:.5}", point.lat, point.lon)));
    if let Some(grid) = mgrs::format(point.lat, point.lon) {
        rows.push(("MGRS", grid));
    }
    if let Some(hae) = point.hae {
        rows.push(("Altitude", format!("{hae:.0} m HAE")));
    }
    if let Some(ce) = point.ce {
        rows.push(("Accuracy", format!("± {ce:.0} m")));
    }
    if let Some(moving) = movement(feature) {
        rows.push(("Moving", moving));
    }
    if let Some(how) = feature.how.as_deref().and_then(how) {
        rows.push(("Source", how.to_string()));
    }
    if let Some(battery) = feature.battery {
        rows.push(("Battery", format!("{battery}%")));
    }
    if let Some(software) = &feature.software {
        rows.push(("Device", software.clone()));
    }
    if !feature.groups.is_empty() {
        rows.push(("Channels", feature.groups.join(", ")));
    }
    if feature.callsign.is_some() {
        rows.push(("UID", feature.uid.clone()));
    }

    rows
}

/// `084° at 5 km/h`, from whichever half of `<track>` was sent.
fn movement(feature: &MapFeature) -> Option<String> {
    let speed = feature
        .speed
        .map(|speed| format!("{:.0} km/h", speed * 3.6));
    let course = feature
        .course
        .map(|course| format!("{:03.0}°", course.rem_euclid(360.0)));

    match (course, speed) {
        (Some(course), Some(speed)) => Some(format!("{course} at {speed}")),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// CoT's `how`, in words. The first letter says who — a human or a machine —
/// and the rest says how; only the ones anybody meets are spelled out.
fn how(how: &str) -> Option<&'static str> {
    match how {
        "m-g" => Some("GPS"),
        "h-e" => Some("Entered by hand"),
        "h-g-i-g-o" => Some("Placed on a map"),
        "m-s" => Some("Simulated"),
        "m-f" => Some("Fused from several sensors"),
        "m-p" => Some("Predicted"),
        other if other.starts_with("m-") => Some("Measured by a machine"),
        other if other.starts_with("h-") => Some("Reported by a person"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::MapPoint;

    use super::*;

    /// A spot marker: a point, a name, and nothing else said about it.
    fn marker() -> MapFeature {
        MapFeature {
            uid: "SPOT-1".to_string(),
            kind: "b-m-p-s-m".to_string(),
            how: None,
            callsign: Some("CCP NORTH".to_string()),
            team: None,
            role: None,
            time: "2026-09-18T12:00:00Z".parse().unwrap(),
            stale: "2026-09-18T12:10:00Z".parse().unwrap(),
            received_at: "2026-09-18T12:00:00Z".parse().unwrap(),
            point: MapPoint {
                lat: 51.5123,
                lon: -0.1312,
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

    /// Somebody carrying a device, which says a great deal more.
    fn person() -> MapFeature {
        MapFeature {
            kind: "a-f-G-U-C".to_string(),
            team: Some("Cyan".to_string()),
            role: Some("Team Lead".to_string()),
            course: Some(84.0),
            speed: Some(1.4),
            battery: Some(74),
            groups: vec!["Blue Team".to_string()],
            ..marker()
        }
    }

    fn terms(feature: &MapFeature) -> Vec<&'static str> {
        facts(feature).into_iter().map(|(term, _)| term).collect()
    }

    #[test]
    fn a_row_is_shown_for_what_the_message_said_and_not_for_what_it_did_not() {
        for term in ["Team", "Moving", "Battery", "Channels"] {
            assert!(terms(&person()).contains(&term), "a person's {term}");
            assert!(!terms(&marker()).contains(&term), "a marker's {term}");
        }

        assert!(terms(&marker()).contains(&"Position"));
    }

    #[test]
    fn movement_reads_as_a_bearing_and_a_road_speed() {
        assert_eq!(movement(&person()).as_deref(), Some("084° at 5 km/h"));
        assert_eq!(movement(&marker()), None);
    }

    #[test]
    fn how_is_said_in_words_down_to_the_family_when_the_member_is_unfamiliar() {
        assert_eq!(how("m-g"), Some("GPS"));
        assert_eq!(how("m-g-n"), Some("Measured by a machine"));
        assert_eq!(how("h-t"), Some("Reported by a person"));
        assert_eq!(how("?"), None);
    }
}
