//! A small, curated catalogue of ATAK preference keys.
//!
//! ATAK has well over a thousand preferences and no published list of them, so
//! this is deliberately short: the ones an operator configuring a server
//! actually reaches for, each with the class it must be sent as. The editor
//! autocompletes from it and warns when a key is unknown — it never refuses
//! one, because a key we have not heard of is far more likely to be a
//! preference we have not catalogued than a mistake.

use rustak_api::{PrefCatalogEntry, PrefClass};

/// `(key, class, description, default)`.
type Entry = (&'static str, PrefClass, &'static str, Option<&'static str>);

/// The keys the editor offers.
const ENTRIES: &[Entry] = &[
    (
        "deviceProfileEnableOnConnect",
        PrefClass::String,
        "Fetch connection and tool profiles on every stream connect. Off in ATAK by default, so nothing else on this list is delivered on connect until it is on.",
        Some("false"),
    ),
    (
        "displayServerConnectionWidget",
        PrefClass::String,
        "Show the server connection indicator on the map.",
        Some("false"),
    ),
    (
        "prefs_enable_channels",
        PrefClass::String,
        "Show the Channels selector, which is how somebody chooses which channels they transmit on.",
        Some("false"),
    ),
    (
        "locationCallsign",
        PrefClass::String,
        "The callsign other people see on their maps.",
        None,
    ),
    (
        "locationTeam",
        PrefClass::String,
        "The team colour, as a word: Cyan, Green, Blue, and so on.",
        Some("Cyan"),
    ),
    (
        "atakRoleType",
        PrefClass::String,
        "The role shown beside the callsign: Team Member, Team Lead, HQ, Sniper, Medic, and so on.",
        Some("Team Member"),
    ),
    (
        "locationUnitType",
        PrefClass::String,
        "The CoT type the device reports itself as.",
        Some("a-f-G-U-C"),
    ),
    (
        "symbologyProvider",
        PrefClass::String,
        "Which edition of MIL-STD-2525 the device authors symbols in: 2525C, 2525D or 2525E. An account's own choice, under Account in this console, is delivered as this key.",
        Some("2525C"),
    ),
    (
        "coord_display_pref",
        PrefClass::String,
        "Which coordinate format the map shows: MGRS, DD, DM, DMS or UTM.",
        Some("MGRS"),
    ),
    (
        "alt_display_pref",
        PrefClass::String,
        "Whether altitude is shown above mean sea level (MSL) or the ellipsoid (HAE).",
        Some("MSL"),
    ),
    (
        "speed_unit_pref",
        PrefClass::String,
        "The unit speeds are shown in.",
        Some("0"),
    ),
    (
        "rab_rng_units_pref",
        PrefClass::String,
        "The unit ranges are shown in.",
        Some("1"),
    ),
    (
        "atakControlOtherUnitsBubble",
        PrefClass::Boolean,
        "Show detail bubbles for other units when they are tapped.",
        Some("true"),
    ),
    (
        "dexControlEnabled",
        PrefClass::Boolean,
        "Allow the device to be driven from an external display.",
        Some("false"),
    ),
    (
        "atakLongPressMap",
        PrefClass::String,
        "What a long press on the map does.",
        None,
    ),
    (
        "loctionReportingStrategy",
        PrefClass::String,
        "How position reports are paced: Dynamic or Constant. The misspelling is ATAK's own and is part of the key.",
        Some("Dynamic"),
    ),
    (
        "constantReportingRateUnreliable",
        PrefClass::Integer,
        "Seconds between position reports over an unreliable link, when reporting is Constant.",
        Some("20"),
    ),
    (
        "dynamicReportingRateStationaryUnreliable",
        PrefClass::Integer,
        "Seconds between position reports while stationary, when reporting is Dynamic.",
        Some("30"),
    ),
    (
        "expireEverything",
        PrefClass::Boolean,
        "Let every map item go stale, rather than keeping pinned items indefinitely.",
        Some("false"),
    ),
    (
        "enableNonStreamingConnections",
        PrefClass::Boolean,
        "Allow non-streaming inputs and outputs to be configured on the device.",
        Some("true"),
    ),
    (
        "network_quic_enabled",
        PrefClass::Boolean,
        "Offer QUIC to servers that advertise it.",
        Some("false"),
    ),
];

/// The catalogue, as the API serves it.
pub fn catalog() -> Vec<PrefCatalogEntry> {
    ENTRIES
        .iter()
        .map(|(key, class, description, default)| PrefCatalogEntry {
            key: (*key).to_string(),
            class: *class,
            description: (*description).to_string(),
            default: default.map(str::to_string),
        })
        .collect()
}

/// The class the catalogue says a key should be sent as, when it knows.
pub fn class_for(key: &str) -> Option<PrefClass> {
    ENTRIES
        .iter()
        .find(|(catalogued, ..)| *catalogued == key)
        .map(|(_, class, ..)| *class)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_has_no_duplicate_keys_and_every_default_fits_its_class() {
        let entries = catalog();
        let mut keys: Vec<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
        keys.sort_unstable();

        let count = keys.len();
        keys.dedup();
        assert_eq!(
            keys.len(),
            count,
            "a duplicate key would autocomplete twice"
        );

        for entry in &entries {
            if let Some(default) = &entry.default {
                assert!(
                    entry.class.accepts(default),
                    "{} has a default its class could not hold",
                    entry.key,
                );
            }

            assert!(!entry.description.is_empty(), "{}", entry.key);
        }
    }

    #[test]
    fn the_preference_everything_depends_on_is_in_it() {
        assert_eq!(
            class_for("deviceProfileEnableOnConnect"),
            Some(PrefClass::String),
        );
        assert_eq!(class_for("somethingWeHaveNotCatalogued"), None);
    }
}
