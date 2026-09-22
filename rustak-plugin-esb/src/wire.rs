//! PowerCheck's JSON, as observed rather than as documented — ESB publishes no
//! schema, so everything here is optional and nothing refuses an unknown key.
//!
//! - `GET /outages` → `{"outageMessage": [{"i": 1, "t": "Fault", "p": {"c": "52.39,-8.85"}}]}`
//! - `GET /outages/<id>/` → one object with the fields of [`Detail`]
//!
//! Traps: a position is a `"lat,lon"` *string*; a missing time is `""`, never
//! null; times are Irish wall-clock (see [`crate::time`]); and `Restored`
//! overwrites whatever the outage's type was before.

use serde::Deserialize;

use crate::outage::{Outage, OutageKind};
use crate::time::parse_dublin;

/// The list: every outage ESB currently shows, in three short keys each.
#[derive(Debug, Default, Deserialize)]
pub struct Listing {
    #[serde(rename = "outageMessage", default)]
    outages: Option<Vec<Listed>>,
}

impl Listing {
    /// The outages that have an id and a position we can read.
    #[must_use]
    pub fn into_outages(self) -> Vec<Outage> {
        self.outages
            .unwrap_or_default()
            .into_iter()
            .filter_map(Listed::into_outage)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct Listed {
    #[serde(rename = "i")]
    id: Option<Scalar>,
    #[serde(rename = "t")]
    kind: Option<String>,
    #[serde(rename = "p")]
    point: Option<Point>,
}

impl Listed {
    fn into_outage(self) -> Option<Outage> {
        let id = self.id?.into_text()?;
        let position = self.point?.position()?;
        let kind = self
            .kind
            .as_deref()
            .map_or(OutageKind::Other, OutageKind::from_wire);

        Some(Outage::new(id, kind, position))
    }
}

/// `{"c": "52.39,-8.85"}`.
#[derive(Debug, Deserialize)]
struct Point {
    c: Option<String>,
}

impl Point {
    fn position(&self) -> Option<(f64, f64)> {
        let (lat, lon) = self.c.as_deref()?.split_once(',')?;
        let (lat, lon) = (
            lat.trim().parse::<f64>().ok()?,
            lon.trim().parse::<f64>().ok()?,
        );

        ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
    }
}

/// A value ESB has been seen to send as either a number or a string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Number(u64),
    Text(String),
}

impl Scalar {
    fn into_text(self) -> Option<String> {
        match self {
            Self::Number(number) => Some(number.to_string()),
            Self::Text(text) => text_of(Some(text)),
        }
    }

    fn into_number(self) -> Option<u64> {
        match self {
            Self::Number(number) => Some(number),
            Self::Text(text) => text.trim().parse().ok(),
        }
    }
}

/// Everything ESB says about one outage.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    outage_type: Option<String>,
    point: Option<Point>,
    location: Option<String>,
    planner_group: Option<String>,
    num_cust_affected: Option<Scalar>,
    start_time: Option<String>,
    est_restore_time: Option<String>,
    restore_time: Option<String>,
    status_message: Option<String>,
    planned_outage_reason: Option<String>,
}

impl Detail {
    /// Fills an outage in. What the detail does not say is left as the list
    /// had it, so a sparse answer never erases a position.
    pub fn apply(self, outage: &mut Outage) {
        if let Some(kind) = text_of(self.outage_type) {
            outage.kind = OutageKind::from_wire(&kind);
        }

        if let Some(position) = self.point.as_ref().and_then(Point::position) {
            outage.position = position;
        }

        outage.location = text_of(self.location);
        outage.depot = text_of(self.planner_group);
        outage.customers = self.num_cust_affected.and_then(Scalar::into_number);
        outage.started_at = self.start_time.as_deref().and_then(parse_dublin);
        outage.estimated_restore_at = self.est_restore_time.as_deref().and_then(parse_dublin);
        outage.restored_at = self.restore_time.as_deref().and_then(parse_dublin);
        outage.status = text_of(self.status_message);
        outage.planned_reason = text_of(self.planned_outage_reason);
    }
}

/// ESB's empty-string sentinel, as the nothing it means.
fn text_of(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-written in the shape the API answers with; not captures.
    const LISTING: &str = include_str!("../tests/fixtures/listing.json");
    const DETAIL: &str = include_str!("../tests/fixtures/detail.json");

    #[test]
    fn the_list_answers_the_outages_that_can_be_put_on_a_map() {
        let listing: Listing = serde_json::from_str(LISTING).expect("it parses");
        let outages = listing.into_outages();

        assert_eq!(outages.len(), 2, "no position or no id is no marker");
        assert_eq!(outages[0].id, "2826455");
        assert_eq!(outages[0].kind, OutageKind::Fault);
        assert_eq!(outages[1].id, "2826460", "an id is an id, quoted or not");
        assert_eq!(outages[1].kind, OutageKind::Planned);
    }

    #[test]
    fn a_list_with_nothing_in_it_is_a_quiet_day_rather_than_an_error() {
        for body in [
            "{}",
            r#"{"outageMessage": null}"#,
            r#"{"outageMessage": []}"#,
        ] {
            let listing: Listing = serde_json::from_str(body).expect(body);

            assert!(listing.into_outages().is_empty(), "{body}");
        }
    }

    #[test]
    fn a_detail_fills_in_what_the_list_left_out() {
        let mut outage = Outage::new("2826455", OutageKind::Fault, (51.8139, -8.3986));
        let detail: Detail = serde_json::from_str(DETAIL).expect("unknown keys are fine");

        detail.apply(&mut outage);

        assert_eq!(outage.location.as_deref(), Some("Carrigaline"));
        assert_eq!(outage.depot.as_deref(), Some("Cork South"));
        assert_eq!(outage.customers, Some(412));
        assert_eq!(outage.started_at, "2026-09-22T13:30:00Z".parse().ok());
        assert_eq!(
            outage.estimated_restore_at,
            "2026-09-22T18:00:00Z".parse().ok()
        );
        assert_eq!(outage.restored_at, None, "an empty string is no time");
        assert_eq!(outage.planned_reason, None);
    }

    #[test]
    fn a_sparse_detail_never_erases_the_position() {
        let mut outage = Outage::new("1", OutageKind::Fault, (51.8139, -8.3986));

        Detail::default().apply(&mut outage);

        assert_eq!(outage.kind, OutageKind::Fault);
        assert!((outage.position.0 - 51.8139).abs() < 1e-9);
    }
}
