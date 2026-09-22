//! The exact symbol a feed asks for, beside the CoT type that implies one.
//!
//! A CoT type *is* a MIL-STD-2525C function path, so a client can always draw
//! a track from its type alone, and that is all a feed says by default. ATAK
//! 5 and later will also take the symbol identification code itself, in a
//! detail: `<__milicon id>` for a single point, and `<__milsym id>` as well
//! when the code is a 2525C one, which is the pair ATAK's own
//! `MilSymDetailHandler` writes "for most robust backwards interoperability".
//! That detail is the only way a device draws a **2525D** symbol for a track
//! somebody else published: a bare type is always drawn from the 2525C tables,
//! whatever edition the device is set to.
//!
//! So this is a deployment choice rather than a constant. A fleet on 2525D
//! sets `symbology = "2525d"` and its ships and aircraft arrive in the edition
//! its own markers are drawn in; everybody else leaves it alone and nothing
//! about the events changes.
//!
//! # Where the 2525D codes come from
//!
//! They are the rows of Esri's open `LegacyMappingTableCtoD` (JMSML,
//! Apache-2.0) for the thirteen function paths [`TrackKind`] can produce. The
//! three that table has no exact row for take the parent entity: civilian sea
//! surface `140000`, an air track with no entity at all, and civilian vehicle
//! `160000`.

use rustak_cot::Event;
use rustak_cot::detail::Element;
use serde::{Deserialize, Serialize};

use super::{Affiliation, AircraftClass, TrackKind, VesselClass};

/// The detail ATAK reads a single point's symbol code from.
const SINGLE_POINT: &str = "__milicon";

/// The older detail, which ATAK still writes beside the first for 2525C.
const MULTI_POINT: &str = "__milsym";

/// Which symbol code a feed writes into its events, if any.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Symbology {
    /// The CoT type and nothing else.
    #[default]
    #[serde(rename = "none")]
    TypeOnly,
    /// The fifteen-letter MIL-STD-2525C code.
    #[serde(rename = "2525c")]
    Milstd2525C,
    /// The twenty-digit MIL-STD-2525D code.
    #[serde(rename = "2525d")]
    Milstd2525D,
}

impl Symbology {
    /// The code for `kind` under `affiliation`, or [`None`] when this feed
    /// leaves the symbol to the type.
    ///
    /// ```
    /// use rustak_client::feed::{Affiliation, Symbology, TrackKind, VesselClass};
    ///
    /// let merchant = TrackKind::Vessel(VesselClass::Merchant);
    ///
    /// assert_eq!(
    ///     Symbology::Milstd2525C.code(merchant, Affiliation::Unknown).as_deref(),
    ///     Some("SUSPXM---------"),
    /// );
    /// assert_eq!(
    ///     Symbology::Milstd2525D.code(merchant, Affiliation::Unknown).as_deref(),
    ///     Some("10013000001401000000"),
    /// );
    /// ```
    #[must_use]
    pub fn code(self, kind: TrackKind, affiliation: Affiliation) -> Option<String> {
        match self {
            Self::TypeOnly => None,
            Self::Milstd2525C => Some(letters(kind, affiliation)),
            Self::Milstd2525D => Some(digits(kind, affiliation)),
        }
    }

    /// Writes the symbol details into an event that was built from `kind`.
    pub fn mark(self, event: &mut Event, kind: TrackKind, affiliation: Affiliation) {
        let Some(code) = self.code(kind, affiliation) else {
            return;
        };

        event
            .detail
            .push(Element::new(SINGLE_POINT).attr("id", code.clone()));

        if self == Self::Milstd2525C {
            event
                .detail
                .push(Element::new(MULTI_POINT).attr("id", code));
        }
    }
}

/// The 2525C code: scheme, affiliation, dimension, status, then the function.
fn letters(kind: TrackKind, affiliation: Affiliation) -> String {
    let mut path = kind.branch().split('-');
    let dimension = path.next().unwrap_or("Z");
    let function: String = path.collect();

    format!(
        "S{}{dimension}P{function:-<11}",
        affiliation.letter().to_ascii_uppercase()
    )
}

/// The 2525D code: version 10, reality, the standard identity, the symbol set,
/// present, no echelon, the entity, and no modifiers.
fn digits(kind: TrackKind, affiliation: Affiliation) -> String {
    let identity = match affiliation {
        Affiliation::Pending => 0,
        Affiliation::Unknown => 1,
        Affiliation::Friend => 3,
        Affiliation::Neutral => 4,
        Affiliation::Hostile => 6,
    };

    let (set, entity) = match kind {
        TrackKind::Vessel(VesselClass::Merchant) => ("30", "140100"),
        TrackKind::Vessel(VesselClass::Fishing) => ("30", "140200"),
        TrackKind::Vessel(VesselClass::LawEnforcement) => ("30", "140300"),
        TrackKind::Vessel(VesselClass::Leisure) => ("30", "140400"),
        TrackKind::Vessel(VesselClass::Military) => ("30", "120000"),
        TrackKind::Vessel(VesselClass::Other) => ("30", "140000"),
        TrackKind::Aircraft(AircraftClass::CivilFixedWing) => ("01", "120100"),
        TrackKind::Aircraft(AircraftClass::CivilRotary) => ("01", "120200"),
        TrackKind::Aircraft(AircraftClass::LighterThanAir) => ("01", "120400"),
        TrackKind::Aircraft(AircraftClass::MilitaryFixedWing) => ("01", "110100"),
        TrackKind::Aircraft(AircraftClass::Uav) => ("01", "110300"),
        TrackKind::Aircraft(AircraftClass::Unknown) => ("01", "000000"),
        TrackKind::GroundVehicle => ("15", "160000"),
    };

    format!("100{identity}{set}0000{entity}0000")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [TrackKind; 4] = [
        TrackKind::Vessel(VesselClass::Other),
        TrackKind::Aircraft(AircraftClass::Unknown),
        TrackKind::Aircraft(AircraftClass::Uav),
        TrackKind::GroundVehicle,
    ];

    fn event(kind: TrackKind) -> Event {
        Event::builder(kind.cot_type(Affiliation::Hostile), "AIS-1").build()
    }

    fn ids(event: &Event, name: &str) -> Vec<String> {
        event
            .detail
            .find_all(name)
            .into_iter()
            .filter_map(|element| element.get("id"))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn each_edition_writes_a_code_of_its_own_shape() {
        let cases = [
            (KINDS[0], "SHSPX----------", "10063000001400000000"),
            (KINDS[1], "SHAP-----------", "10060100000000000000"),
            (KINDS[2], "SHAPMFQ--------", "10060100001103000000"),
            (KINDS[3], "SHGPEVC--------", "10061500001600000000"),
        ];

        for (kind, letters, digits) in cases {
            assert_eq!(
                Symbology::Milstd2525C
                    .code(kind, Affiliation::Hostile)
                    .as_deref(),
                Some(letters)
            );
            assert_eq!(
                Symbology::Milstd2525D
                    .code(kind, Affiliation::Hostile)
                    .as_deref(),
                Some(digits)
            );
        }
    }

    #[test]
    fn a_feed_that_says_nothing_leaves_the_event_alone() {
        let mut marked = event(KINDS[0]);
        Symbology::TypeOnly.mark(&mut marked, KINDS[0], Affiliation::Hostile);

        assert_eq!(marked, event(KINDS[0]));
        assert_eq!(Symbology::default(), Symbology::TypeOnly);
    }

    #[test]
    fn the_details_are_the_ones_atak_writes_for_that_edition() {
        let mut older = event(KINDS[2]);
        Symbology::Milstd2525C.mark(&mut older, KINDS[2], Affiliation::Hostile);
        assert_eq!(ids(&older, SINGLE_POINT), ["SHAPMFQ--------"]);
        assert_eq!(ids(&older, MULTI_POINT), ["SHAPMFQ--------"]);

        let mut newer = event(KINDS[2]);
        Symbology::Milstd2525D.mark(&mut newer, KINDS[2], Affiliation::Hostile);
        assert_eq!(ids(&newer, SINGLE_POINT), ["10060100001103000000"]);
        assert!(ids(&newer, MULTI_POINT).is_empty());
    }

    #[test]
    fn a_configuration_file_names_an_edition_the_way_the_console_does() {
        for (text, expected) in [
            ("\"none\"", Symbology::TypeOnly),
            ("\"2525c\"", Symbology::Milstd2525C),
            ("\"2525d\"", Symbology::Milstd2525D),
        ] {
            assert_eq!(serde_json::from_str::<Symbology>(text).ok(), Some(expected));
        }

        assert!(serde_json::from_str::<Symbology>("\"2525e\"").is_err());
    }
}
