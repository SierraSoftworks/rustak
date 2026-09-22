//! What a track *is*, and the CoT type that says so.
//!
//! CoT types are the MIL-STD-2525 symbol hierarchy written as a dotted-with-
//! hyphens path: `a-<affiliation>-<battle dimension>-<function>…`. The leading
//! `a` is the "atom" category (a thing that exists at a place), the second
//! letter is the affiliation, and everything after it walks down the symbol
//! tree — `S` sea surface, `A` air, `G` ground — one level per hyphen.
//!
//! Only the arms an information feed needs are modelled here. A feed never
//! knows more than "this is a cargo ship" or "this is a light aeroplane": the
//! finer 2525 distinctions need an order of battle that AIS and ADS-B do not
//! carry, and a plugin that guessed at one would be putting a claim on an
//! operator's map that nothing in the data supports.

use serde::{Deserialize, Serialize};

/// Which side a track is on, as far as the feed knows.
///
/// Open data says nothing about intent, so [`Unknown`](Affiliation::Unknown) is
/// the honest default and the one every source plugin ships with. An operator
/// who knows better sets it per deployment — an AIS receiver watching a
/// friendly harbour is a reasonable `Friend`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Affiliation {
    /// `u` — nothing is claimed about this track.
    #[default]
    Unknown,
    /// `f` — friendly.
    Friend,
    /// `n` — neutral.
    Neutral,
    /// `h` — hostile.
    Hostile,
    /// `p` — pending: being evaluated, not yet decided.
    Pending,
}

impl Affiliation {
    /// The single letter this affiliation occupies in a CoT type.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::Unknown => 'u',
            Self::Friend => 'f',
            Self::Neutral => 'n',
            Self::Hostile => 'h',
            Self::Pending => 'p',
        }
    }
}

/// What kind of vessel an AIS report describes.
///
/// AIS ship-type codes are a hundred numbers in nine bands; these are the bands
/// an operator can act on. Mapping the code to one of these is the AIS plugin's
/// job, not this module's — the codes are a property of AIS, and an ADS-B
/// aircraft carrier report would have no use for them.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VesselClass {
    /// Cargo, tanker, tug, pilot and the rest of the working traffic.
    Merchant,
    /// Fishing vessels.
    Fishing,
    /// Sailing and pleasure craft.
    Leisure,
    /// Law enforcement, search and rescue, and other state patrol craft.
    LawEnforcement,
    /// A military vessel — a combatant rather than a civil hull.
    Military,
    /// A vessel whose type the report did not say.
    #[default]
    Other,
}

/// What kind of aircraft an ADS-B report describes.
///
/// ADS-B emitter categories (A1–A7, B1–B7, C1–C3) are finer than this, and the
/// ADS-B plugin collapses them: weight class tells an operator nothing they can
/// act on, whereas "is it a helicopter" and "is it a drone" do.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftClass {
    /// A civil fixed-wing aeroplane — the overwhelming majority of traffic.
    CivilFixedWing,
    /// A civil rotorcraft.
    CivilRotary,
    /// A balloon or airship.
    LighterThanAir,
    /// A military fixed-wing aircraft.
    MilitaryFixedWing,
    /// An unmanned aerial vehicle.
    Uav,
    /// An aircraft whose category the report did not say.
    #[default]
    Unknown,
}

/// What a [`Track`](super::Track) is.
///
/// One enum across both feeds, because the publisher, the map and the operator
/// treat a ship and an aeroplane the same way: a moving thing with an id, a
/// position and a symbol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Something on the water.
    Vessel(VesselClass),
    /// Something in the air.
    Aircraft(AircraftClass),
    /// Something on the ground: an airport service vehicle, a tug on the apron.
    GroundVehicle,
}

impl Default for TrackKind {
    fn default() -> Self {
        Self::Vessel(VesselClass::Other)
    }
}

impl TrackKind {
    /// The CoT type for this kind under the given affiliation, e.g.
    /// `a-u-S-X-M` for an unknown merchant vessel.
    ///
    /// ```
    /// use rustak_client::feed::{Affiliation, TrackKind, VesselClass};
    ///
    /// assert_eq!(
    ///     TrackKind::Vessel(VesselClass::Merchant).cot_type(Affiliation::Unknown),
    ///     "a-u-S-X-M",
    /// );
    /// ```
    #[must_use]
    pub fn cot_type(self, affiliation: Affiliation) -> String {
        format!("a-{}-{}", affiliation.letter(), self.branch())
    }

    /// The part of the type below the affiliation: the 2525 path itself.
    pub(super) const fn branch(self) -> &'static str {
        match self {
            // Sea surface (`S`), civil (`X`) unless the hull is a combatant.
            Self::Vessel(VesselClass::Merchant) => "S-X-M",
            Self::Vessel(VesselClass::Fishing) => "S-X-F",
            Self::Vessel(VesselClass::Leisure) => "S-X-R",
            Self::Vessel(VesselClass::LawEnforcement) => "S-X-L",
            // A combatant is a *military* sea-surface track (`S-C`), which is a
            // different branch of the tree rather than a civil one with a flag.
            Self::Vessel(VesselClass::Military) => "S-C",
            // Sea surface, nothing more said.
            Self::Vessel(VesselClass::Other) => "S-X",
            // Air (`A`), civil (`C`) fixed wing (`F`) / rotary (`H`) / lighter
            // than air (`L`).
            Self::Aircraft(AircraftClass::CivilFixedWing) => "A-C-F",
            Self::Aircraft(AircraftClass::CivilRotary) => "A-C-H",
            Self::Aircraft(AircraftClass::LighterThanAir) => "A-C-L",
            Self::Aircraft(AircraftClass::MilitaryFixedWing) => "A-M-F",
            // A drone is a fixed-wing military air track qualified as unmanned.
            Self::Aircraft(AircraftClass::Uav) => "A-M-F-Q",
            // Air, nothing more said: the honest answer for a bare position
            // report with no category in it.
            Self::Aircraft(AircraftClass::Unknown) => "A",
            // Ground (`G`) equipment (`E`) vehicle (`V`) civilian (`C`).
            Self::GroundVehicle => "G-E-V-C",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_affiliation_has_its_letter() {
        let letters: Vec<char> = [
            Affiliation::Unknown,
            Affiliation::Friend,
            Affiliation::Neutral,
            Affiliation::Hostile,
            Affiliation::Pending,
        ]
        .into_iter()
        .map(Affiliation::letter)
        .collect();

        assert_eq!(letters, ['u', 'f', 'n', 'h', 'p']);
        assert_eq!(Affiliation::default(), Affiliation::Unknown);
    }

    #[test]
    fn every_vessel_class_maps_to_its_2525_branch() {
        // Spelled out arm by arm rather than looped: these strings are the
        // contract the AIS and ADS-B plugins are written against, and a table
        // that generated them would fail in exactly the same way as the code.
        let cases = [
            (VesselClass::Merchant, "a-u-S-X-M"),
            (VesselClass::Fishing, "a-u-S-X-F"),
            (VesselClass::Leisure, "a-u-S-X-R"),
            (VesselClass::LawEnforcement, "a-u-S-X-L"),
            (VesselClass::Military, "a-u-S-C"),
            (VesselClass::Other, "a-u-S-X"),
        ];

        for (class, expected) in cases {
            assert_eq!(
                TrackKind::Vessel(class).cot_type(Affiliation::Unknown),
                expected,
                "{class:?}",
            );
        }
    }

    #[test]
    fn every_aircraft_class_maps_to_its_2525_branch() {
        let cases = [
            (AircraftClass::CivilFixedWing, "a-u-A-C-F"),
            (AircraftClass::CivilRotary, "a-u-A-C-H"),
            (AircraftClass::LighterThanAir, "a-u-A-C-L"),
            (AircraftClass::MilitaryFixedWing, "a-u-A-M-F"),
            (AircraftClass::Uav, "a-u-A-M-F-Q"),
            (AircraftClass::Unknown, "a-u-A"),
        ];

        for (class, expected) in cases {
            assert_eq!(
                TrackKind::Aircraft(class).cot_type(Affiliation::Unknown),
                expected,
                "{class:?}",
            );
        }
    }

    #[test]
    fn a_ground_vehicle_is_civilian_equipment() {
        assert_eq!(
            TrackKind::GroundVehicle.cot_type(Affiliation::Unknown),
            "a-u-G-E-V-C"
        );
    }

    #[test]
    fn the_affiliation_is_the_only_thing_that_varies_between_deployments() {
        let kind = TrackKind::Aircraft(AircraftClass::CivilFixedWing);

        assert_eq!(kind.cot_type(Affiliation::Friend), "a-f-A-C-F");
        assert_eq!(kind.cot_type(Affiliation::Neutral), "a-n-A-C-F");
        assert_eq!(kind.cot_type(Affiliation::Hostile), "a-h-A-C-F");
        assert_eq!(kind.cot_type(Affiliation::Pending), "a-p-A-C-F");
    }

    #[test]
    fn a_kind_round_trips_through_the_replay_fixture_format() {
        let kind = TrackKind::Vessel(VesselClass::Fishing);
        let json = serde_json::to_string(&kind).unwrap();

        assert_eq!(json, r#"{"vessel":"fishing"}"#);
        assert_eq!(
            serde_json::from_str::<TrackKind>(r#""ground_vehicle""#).unwrap(),
            TrackKind::GroundVehicle,
        );
        assert_eq!(serde_json::from_str::<TrackKind>(&json).unwrap(), kind);
    }
}
