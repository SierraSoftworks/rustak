//! One power outage, and the CoT marker it becomes.
//!
//! An outage does not move and has no allegiance, so it is not an atom with a
//! MIL-STD-2525 symbol: it is a **spot-map marker** (`b-m-p-s-m`), which every
//! TAK client draws as a coloured dot with a label, and the colour carries the
//! one thing worth seeing from across a map.
//!
//! | Kind | Colour |
//! |---|---|
//! | `fault` | red |
//! | `planned` | orange |
//! | `restored` | green |
//! | `other` | white |

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_cot::detail::{Contact, Element, Remarks};
use rustak_cot::{CotTime, Event};
use serde::{Deserialize, Serialize};

use crate::time::zulu;

/// A spot-map marker.
pub const COT_TYPE: &str = "b-m-p-s-m";

/// Machine-reported: nobody placed this marker by hand, and it is no GPS fix.
const HOW: &str = "m-r";

/// What goes in front of an outage id to make a CoT uid.
pub const UID_PREFIX: &str = "ESB-";

/// What ESB says an outage is.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutageKind {
    /// An unplanned loss of supply.
    Fault,
    /// Scheduled works, possibly still in the future.
    Planned,
    /// Supply is back; ESB lists these for a few hours afterwards.
    Restored,
    /// A type this plugin has not seen before.
    Other,
}

impl OutageKind {
    /// Every kind, which is what a settings file that says nothing shows.
    pub const ALL: [Self; 4] = [Self::Fault, Self::Planned, Self::Restored, Self::Other];

    /// Reads ESB's own word for it, forgivingly.
    #[must_use]
    pub fn from_wire(value: &str) -> Self {
        let value = value.trim().to_ascii_lowercase();

        if value.starts_with("fault") {
            Self::Fault
        } else if value.starts_with("plan") {
            Self::Planned
        } else if value.starts_with("restor") {
            Self::Restored
        } else {
            Self::Other
        }
    }

    /// What an operator reads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fault => "Fault",
            Self::Planned => "Planned",
            Self::Restored => "Restored",
            Self::Other => "Outage",
        }
    }

    /// The marker colour, as the signed ARGB integer TAK writes.
    #[must_use]
    pub const fn argb(self) -> i32 {
        match self {
            Self::Fault => -65_536,
            Self::Planned => -35_072,
            Self::Restored => -16_711_936,
            Self::Other => -1,
        }
    }
}

/// One outage as a source reported it. Also the replay fixture's line format.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Outage {
    /// ESB's outage id, without the uid prefix.
    pub id: String,
    /// What ESB says it is.
    pub kind: OutageKind,
    /// Latitude and longitude, decimal degrees. Where the fault is pinned,
    /// which is not the same as who is off supply.
    pub position: (f64, f64),
    /// ESB's name for the place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The ESB depot dealing with it (`plannerGroup`); not a county.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depot: Option<String>,
    /// Customers off supply, as last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customers: Option<u64>,
    /// When supply was lost, or the works begin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// When ESB expects supply back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_restore_at: Option<DateTime<Utc>>,
    /// When supply came back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_at: Option<DateTime<Utc>>,
    /// ESB's status message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why planned works are happening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_reason: Option<String>,
}

impl Outage {
    /// An outage known only from the list: an id, a kind and a position.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: OutageKind, position: (f64, f64)) -> Self {
        Self {
            id: id.into(),
            kind,
            position,
            location: None,
            depot: None,
            customers: None,
            started_at: None,
            estimated_restore_at: None,
            restored_at: None,
            status: None,
            planned_reason: None,
        }
    }

    /// The uid on the map.
    #[must_use]
    pub fn uid(&self) -> String {
        format!("{UID_PREFIX}{}", self.id)
    }

    /// Whether ESB has said everything it is going to about this one.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        matches!(self.kind, OutageKind::Restored) && self.restored_at.is_some()
    }

    /// The marker's label: `Fault: Carrigaline (412)`.
    #[must_use]
    pub fn callsign(&self) -> String {
        let place = self.location.as_deref().unwrap_or(&self.id);

        match self.customers {
            Some(customers) => format!("{}: {place} ({customers})", self.kind.label()),
            None => format!("{}: {place}", self.kind.label()),
        }
    }

    /// The lines an operator reads when they tap the marker.
    #[must_use]
    pub fn remarks(&self) -> String {
        let lines = [
            ("Type", Some(self.kind.label().to_string())),
            ("Location", self.location.clone()),
            ("Customers affected", self.customers.map(|n| n.to_string())),
            ("Started", self.started_at.map(zulu)),
            ("Estimated restore", self.estimated_restore_at.map(zulu)),
            ("Restored", self.restored_at.map(zulu)),
            ("Status", self.status.clone()),
            ("Reason", self.planned_reason.clone()),
            ("Depot", self.depot.clone()),
            ("Source", Some("ESB Networks PowerCheck".to_string())),
        ];

        lines
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| format!("{key}: {value}")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The event a client draws, valid from `now` for `stale`.
    #[must_use]
    pub fn to_event(&self, now: DateTime<Utc>, stale: Duration) -> Event {
        let time = CotTime::from_datetime(now);
        let argb = self.kind.argb().to_string();

        Event::builder(COT_TYPE, self.uid())
            .how(HOW)
            .point(self.position.0, self.position.1)
            .time(time)
            .start(time)
            .stale(time.stale_after(stale))
            // No endpoint: a marker is a thing on the map, not a chat peer.
            .typed(&Contact::new(self.callsign()))
            .typed(&Remarks {
                text: self.remarks(),
                ..Remarks::default()
            })
            .push(Element::new("color").attr("argb", argb.clone()))
            .push(Element::new("usericon").attr(
                "iconsetpath",
                format!("COT_MAPPING_SPOTMAP/{COT_TYPE}/{argb}"),
            ))
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn fault() -> Outage {
        Outage {
            location: Some("Carrigaline".to_string()),
            customers: Some(412),
            started_at: "2026-09-22T13:30:00Z".parse().ok(),
            status: Some("Crew assigned".to_string()),
            ..Outage::new("2826455", OutageKind::Fault, (51.8139, -8.3986))
        }
    }

    #[rstest]
    #[case("Fault", OutageKind::Fault)]
    #[case("fault", OutageKind::Fault)]
    #[case("Planned", OutageKind::Planned)]
    #[case("Planned Outage", OutageKind::Planned)]
    #[case(" Restored ", OutageKind::Restored)]
    #[case("Something new", OutageKind::Other)]
    fn esbs_word_for_an_outage_is_read_forgivingly(#[case] wire: &str, #[case] kind: OutageKind) {
        assert_eq!(OutageKind::from_wire(wire), kind);
    }

    #[test]
    fn an_outage_is_a_labelled_spot_marker_where_esb_pinned_it() {
        let now = "2026-09-22T14:00:00Z".parse().expect("an instant");

        let event = fault().to_event(now, Duration::from_secs(900));

        assert_eq!(event.uid, "ESB-2826455");
        assert_eq!(event.r#type, COT_TYPE);
        assert!((event.point.lat - 51.8139).abs() < 1e-9);
        assert!((event.point.lon - -8.3986).abs() < 1e-9);
        assert_eq!(event.stale.millis() - event.time.millis(), 900_000);

        let callsign = event.callsign().expect("a label");
        assert!(callsign.contains("Carrigaline"), "{callsign}");
        assert!(callsign.contains("412"), "{callsign}");
        assert_eq!(event.endpoint(), None);
    }

    #[test]
    fn the_remarks_carry_what_esb_said_and_nothing_it_did_not() {
        let event = fault().to_event(Utc::now(), Duration::from_secs(900));
        let remarks = event.detail.get::<Remarks>().expect("remarks").text;

        assert!(remarks.contains("Customers affected: 412"), "{remarks}");
        assert!(remarks.contains("Started: 2026-09-22 13:30Z"), "{remarks}");
        assert!(remarks.contains("Status: Crew assigned"), "{remarks}");
        assert!(!remarks.contains("Restored:"), "{remarks}");
    }

    #[rstest]
    #[case(OutageKind::Fault, "-65536")]
    #[case(OutageKind::Planned, "-35072")]
    #[case(OutageKind::Restored, "-16711936")]
    fn the_marker_colour_says_what_kind_of_outage_it_is(
        #[case] kind: OutageKind,
        #[case] argb: &str,
    ) {
        let event = Outage::new("1", kind, (53.0, -8.0)).to_event(Utc::now(), Duration::ZERO);

        let color = event.detail.find("color").expect("a colour");
        let icon = event.detail.find("usericon").expect("an icon");

        assert_eq!(color.get("argb"), Some(argb));
        assert!(
            icon.get("iconsetpath")
                .is_some_and(|path| path.ends_with(argb))
        );
    }

    #[test]
    fn only_a_restoration_with_a_time_is_final() {
        let mut outage = Outage::new("1", OutageKind::Restored, (53.0, -8.0));
        assert!(!outage.is_final(), "ESB sends the time a little later");

        outage.restored_at = Some(Utc::now());
        assert!(outage.is_final());
    }
}
