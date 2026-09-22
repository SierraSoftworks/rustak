//! One relayed CoT event, as the map draws it.
//!
//! The same conversion serves both reads: the snapshot parses a stored row's
//! XML into an [`Event`] and the feed is handed one by the router, and both
//! end up here, so a marker cannot look different depending on whether the
//! page was open when it arrived.

use chrono::{DateTime, Utc};
use rustak_api::{MapFeature, MapPoint};
use rustak_cot::detail::{Remarks, Status, Takv, Track};
use rustak_cot::event::Point;
use rustak_cot::types::{cot_type, is_chat};
use rustak_cot::{Event, xml};

use crate::cot_store::LatestRow;
use crate::prelude::*;

use super::shape;

/// Whether a CoT type is something a map draws.
///
/// Decided by what is left out rather than what is let in, because the set of
/// things TAK clients put on a map is open — atoms, spot markers, routes,
/// drawings, alerts, whatever a plugin invents — and the set of things that
/// are *conversation* rather than *situation* is small and known: tasking and
/// control (`t-`), chat (`b-t-f`), file transfer (`b-f-t`) and replies (`y-`).
pub fn drawable(kind: &str) -> bool {
    !(kind.starts_with("t-")
        || kind.starts_with("y-")
        || kind.starts_with("b-f-t")
        || is_chat(kind))
}

/// The uids a `t-x-d-d` asks every map to forget.
///
/// A client deleting a marker sends one of these with a `<link uid>` naming
/// what is gone; the event's own uid is a throwaway.
pub fn removals(event: &Event) -> Vec<String> {
    if event.r#type != cot_type::DISCONNECT {
        return Vec::new();
    }

    event
        .detail
        .find_all("link")
        .into_iter()
        .filter_map(|link| link.get("uid"))
        .map(str::to_owned)
        .collect()
}

/// A stored row as a feature, or [`None`] when its XML no longer parses.
///
/// A row that cannot be read still *lists* in the CoT browser, because there
/// the bytes are the point. Here the point is the point, and the row does not
/// keep one outside the XML.
pub fn from_row(row: &LatestRow, index: &GroupIndex) -> Option<MapFeature> {
    let event = xml::parse_str(&row.xml).ok()?;
    let groups = GroupSet::from_bytes(&row.group_bits)
        .map(|sender| channel_names(&sender, index))
        .unwrap_or_default();

    Some(from_event(&event, row.received_at, groups))
}

/// A relayed event as a feature.
pub fn from_event(event: &Event, received_at: DateTime<Utc>, groups: Vec<String>) -> MapFeature {
    let group = event.group();
    let track = event.detail.get::<Track>();

    MapFeature {
        uid: event.uid.clone(),
        kind: event.r#type.clone(),
        how: event.how.clone(),
        callsign: event.callsign().map(str::to_owned),
        team: group.as_ref().map(|group| group.name.clone()),
        role: group.as_ref().map(|group| group.role.clone()),
        time: event.time.to_datetime().unwrap_or(received_at),
        stale: event.stale.to_datetime().unwrap_or(received_at),
        received_at,
        point: point(&event.point),
        shape: shape::of(event),
        course: track.as_ref().and_then(|track| known(track.course)),
        speed: track.as_ref().and_then(|track| known(track.speed)),
        battery: event.detail.get::<Status>().map(|status| status.battery),
        remarks: event
            .detail
            .get::<Remarks>()
            .map(|remarks| remarks.text.trim().to_owned())
            .filter(|text| !text.is_empty()),
        software: event.takv().as_ref().and_then(software),
        sidc: symbol(event),
        groups,
    }
}

/// The symbol code the sender asked for, if it wrote one.
///
/// `<__milicon id>` is what ATAK 5 writes for a single point and `<__milsym
/// id>` what it writes for a drawing — and beside the first, for a 2525C code,
/// so that older clients still find it. The single-point one is looked at
/// first because it is the one a 2525D device fills in.
fn symbol(event: &Event) -> Option<String> {
    ["__milicon", "__milsym"]
        .into_iter()
        .filter_map(|name| event.detail.find(name)?.get("id"))
        .find_map(rustak_api::map::sidc)
}

/// The names of the channels a sender was publishing into.
///
/// A bit with no name is a channel deleted since; it is left out rather than
/// rendered as a number nobody can act on.
pub fn channel_names(sender: &GroupSet, index: &GroupIndex) -> Vec<String> {
    sender
        .names(index, Direction::In)
        .into_iter()
        .map(|name| name.as_str().to_owned())
        .collect()
}

fn point(point: &Point) -> MapPoint {
    MapPoint {
        lat: point.lat,
        lon: point.lon,
        hae: measured(point.hae, Point::UNKNOWN_HAE),
        ce: measured(point.ce, Point::UNKNOWN_CE),
        le: measured(point.le, Point::UNKNOWN_LE),
    }
}

/// A value, unless it is CoT's way of saying there is none.
fn measured(value: f64, unknown: f64) -> Option<f64> {
    known(value).filter(|value| *value < unknown)
}

/// A number somebody can be shown. JSON has no way to spell the others.
fn known(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

/// `ATAK-CIV 5.2.0 · Pixel 8`, from whichever parts of `<takv>` were filled in.
fn software(takv: &Takv) -> Option<String> {
    let app = [takv.platform.trim(), takv.version.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let described = [app.as_str(), takv.device.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");

    (!described.is_empty()).then_some(described)
}

#[cfg(test)]
mod tests {
    use rustak_cot::detail::{Contact, Element, Group};

    use super::*;

    fn received() -> DateTime<Utc> {
        "2026-09-18T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn situation_is_drawn_and_conversation_is_not() {
        for kind in ["a-f-G-U-C", "b-m-p-s-m", "b-m-r", "u-d-f", "b-a-o-tbl"] {
            assert!(drawable(kind), "{kind} belongs on a map");
        }

        for kind in ["t-x-c-t", "t-x-d-d", "b-t-f", "b-t-f-d", "b-f-t-r", "y-a-r"] {
            assert!(!drawable(kind), "{kind} does not belong on a map");
        }
    }

    #[test]
    fn a_feature_carries_what_the_pop_over_shows() {
        let event = Event::builder("a-f-G-U-C", "ANDROID-1")
            .how("m-g")
            .point(51.5, -0.12)
            .typed(&Contact::new("ALPHA"))
            .typed(&Group::new("Cyan", "Team Lead"))
            .push(
                Element::new("track")
                    .attr("course", "90.5")
                    .attr("speed", "1.5"),
            )
            .push(Element::new("status").attr("battery", "82"))
            .build();

        let feature = from_event(&event, received(), vec!["Blue".to_string()]);

        assert_eq!(feature.callsign.as_deref(), Some("ALPHA"));
        assert_eq!(feature.team.as_deref(), Some("Cyan"));
        assert_eq!(feature.course, Some(90.5));
        assert_eq!(feature.battery, Some(82));
        assert_eq!(feature.groups, ["Blue"]);
        assert_eq!((feature.point.lat, feature.point.lon), (51.5, -0.12));
    }

    #[test]
    fn the_unknown_sentinel_never_reaches_a_page() {
        let event = Event::builder("a-f-G-U-C", "ANDROID-1")
            .point(51.5, -0.12)
            .build();

        let feature = from_event(&event, received(), Vec::new());

        assert_eq!(feature.point.hae, None);
        assert_eq!(feature.point.ce, None);
    }

    #[test]
    fn a_number_json_cannot_spell_is_left_out_rather_than_sent() {
        let event = Event::builder("a-f-A-C-F", "ICAO-1")
            .point(51.5, -0.12)
            .push(
                Element::new("track")
                    .attr("course", "NaN")
                    .attr("speed", "inf"),
            )
            .build();

        let feature = from_event(&event, received(), Vec::new());

        assert_eq!(feature.course, None);
        assert_eq!(feature.speed, None);
    }

    #[test]
    fn the_device_is_described_from_whichever_parts_of_takv_were_filled_in() {
        let takv = |platform: &str, version: &str, device: &str| Takv {
            platform: platform.to_string(),
            version: version.to_string(),
            device: device.to_string(),
            ..Takv::default()
        };

        for (given, described) in [
            (
                takv("ATAK-CIV", "5.2.0", "Pixel 8"),
                Some("ATAK-CIV 5.2.0 · Pixel 8"),
            ),
            (takv("ATAK-CIV", "", ""), Some("ATAK-CIV")),
            (takv("", "", "Pixel 8"), Some("Pixel 8")),
            (takv(" ", "", ""), None),
        ] {
            assert_eq!(software(&given).as_deref(), described, "{given:?}");
        }
    }

    /// A stored row whose sender was publishing into the channel at bit 2.
    fn row(xml: &str) -> LatestRow {
        let mut groups = GroupSet::new();
        groups.set(2, Direction::In);

        LatestRow {
            uid: "UID-A".to_string(),
            kind: "a-f-G-U-C".to_string(),
            callsign: Some("ALPHA".to_string()),
            user_id: None,
            device_id: None,
            group_bits: groups.to_bytes(),
            time: received(),
            stale: received(),
            xml: xml.to_string(),
            received_at: received(),
        }
    }

    #[test]
    fn a_stored_row_is_drawn_from_its_xml_with_its_channels_named() {
        let mut index = GroupIndex::new();
        index.insert(2, GroupName::parse("Blue").unwrap());

        let stored = row("<event version=\"2.0\" uid=\"UID-A\" type=\"a-f-G-U-C\" \
             time=\"2026-09-18T12:00:00.000Z\" start=\"2026-09-18T12:00:00.000Z\" \
             stale=\"2026-09-18T12:02:00.000Z\" how=\"m-g\">\
             <point lat=\"51.5\" lon=\"-0.12\" hae=\"35.0\" ce=\"9999999.0\" le=\"9999999.0\"/>\
             <detail><takv platform=\"ATAK-CIV\" version=\"5.2.0\" device=\"Pixel 8\" os=\"34\"/>\
             <remarks>  </remarks></detail></event>");

        let feature = from_row(&stored, &index).expect("a row that parses is drawn");

        assert_eq!(feature.groups, ["Blue"]);
        assert_eq!(feature.point.hae, Some(35.0));
        assert_eq!(
            feature.software.as_deref(),
            Some("ATAK-CIV 5.2.0 · Pixel 8")
        );
        assert_eq!(
            feature.remarks, None,
            "a remark that says nothing is not one"
        );
    }

    #[test]
    fn a_stored_row_that_no_longer_parses_is_not_drawn() {
        assert_eq!(from_row(&row("<event"), &GroupIndex::new()), None);
    }

    #[test]
    fn a_delete_names_what_is_gone_in_its_link() {
        let event = Event::builder(cot_type::DISCONNECT, "throwaway")
            .push(
                Element::new("link")
                    .attr("uid", "MARKER-1")
                    .attr("relation", "p-p"),
            )
            .build();

        assert_eq!(removals(&event), ["MARKER-1"]);
        assert!(removals(&Event::builder("a-f-G-U-C", "ANDROID-1").build()).is_empty());
    }

    #[test]
    fn the_symbol_a_sender_asked_for_is_carried_in_whichever_detail_it_used() {
        const LETTERS: &str = "SFAPMH---------";
        const DIGITS: &str = "10030100001102000000";

        /// The `(detail, id)` pairs an event carries, and the code that is drawn.
        type Case<'a> = (&'a [(&'a str, &'a str)], Option<&'a str>);

        let cases: [Case<'_>; 5] = [
            (&[], None),
            (&[("__milicon", DIGITS)], Some(DIGITS)),
            (&[("__milsym", LETTERS)], Some(LETTERS)),
            (
                &[("__milsym", LETTERS), ("__milicon", DIGITS)],
                Some(DIGITS),
            ),
            (
                &[("__milicon", "a-f-A-M-H"), ("__milsym", LETTERS)],
                Some(LETTERS),
            ),
        ];

        for (details, expected) in cases {
            let event = details
                .iter()
                .fold(
                    Event::builder("a-f-A-M-H", "HELO-1"),
                    |builder, (name, id)| builder.push(Element::new(*name).attr("id", *id)),
                )
                .build();

            assert_eq!(
                from_event(&event, received(), Vec::new()).sidc.as_deref(),
                expected,
                "{details:?}"
            );
        }
    }
}
