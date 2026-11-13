//! A catalogue as a picker shows it, for one affiliation.
//!
//! A catalogue row says what something is and not whose; a CoT type and a
//! symbol code say both. So a tree is built *for* an affiliation — every value
//! in it is complete, and every preview is drawn in that affiliation's frame
//! and colour, which is most of what somebody recognises a symbol by.

use rustak_api::Symbology;

use crate::components::{Tree, TreeEntry};

use super::codes::{self, Affiliation};

/// The types that are not atoms and are still worth offering: a point
/// somebody drops on a map, and the two alerts a console raises.
const MARKERS: &[(&str, &str)] = &[
    ("b-m-p-s-m", "Spot marker"),
    ("b-m-p-w", "Waypoint"),
    ("b-a-o-tbl", "Alert · troops in contact"),
    ("b-a-o-can", "Alert · cancel"),
];

/// Every CoT type worth offering, as `whose`.
///
/// The markers come first because they are what somebody placing a point
/// wants most often; the rest is MIL-STD-2525C's warfighting hierarchy, which
/// is what CoT's atom types were laid out on.
pub fn types(rows: &[(String, Vec<String>)], whose: Affiliation) -> Tree {
    Tree::build(type_entries(rows, whose))
}

fn type_entries(rows: &[(String, Vec<String>)], whose: Affiliation) -> Vec<TreeEntry> {
    let markers = MARKERS.iter().map(|(kind, label)| TreeEntry {
        path: vec!["Markers".to_string(), (*label).to_string()],
        value: Some((*kind).to_string()),
        detail: Some((*kind).to_string()),
        icon: None,
    });
    let atoms = rows.iter().map(|(row, names)| {
        let kind = codes::cot_type(whose, row);

        TreeEntry {
            path: names.clone(),
            value: Some(kind.clone()),
            detail: Some(kind),
            icon: Some(codes::sidc(Symbology::Milstd2525C, whose, row)),
        }
    });

    markers.chain(atoms).collect()
}

/// Every symbol of `edition`, as `whose`.
pub fn symbols(rows: &[(String, Vec<String>)], edition: Symbology, whose: Affiliation) -> Tree {
    Tree::build(symbol_entries(rows, edition, whose))
}

fn symbol_entries(
    rows: &[(String, Vec<String>)],
    edition: Symbology,
    whose: Affiliation,
) -> Vec<TreeEntry> {
    rows.iter()
        .map(|(row, names)| {
            let code = codes::sidc(edition, whose, row);

            TreeEntry {
                path: names.clone(),
                value: Some(code.clone()),
                detail: Some(code.clone()),
                icon: Some(code),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: Affiliation = codes::AFFILIATIONS[6];

    fn rows(listed: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        listed
            .iter()
            .map(|(row, names)| {
                (
                    (*row).to_string(),
                    names.iter().map(|name| (*name).to_string()).collect(),
                )
            })
            .collect()
    }

    fn valued<'a>(entries: &'a [TreeEntry], value: &str) -> &'a TreeEntry {
        entries
            .iter()
            .find(|entry| entry.value.as_deref() == Some(value))
            .unwrap_or_else(|| panic!("no entry valued {value}"))
    }

    #[test]
    fn a_type_is_drawn_as_whose_it_is() {
        let entries = type_entries(
            &rows(&[
                ("G------", &["Ground track"]),
                ("GUCI---", &["Ground track", "Unit", "Combat", "Infantry"]),
            ]),
            HOSTILE,
        );

        let infantry = valued(&entries, "a-h-G-U-C-I");
        assert_eq!(infantry.path.last().map(String::as_str), Some("Infantry"));
        assert_eq!(infantry.icon.as_deref(), Some("SHGPUCI--------"));

        // A marker is no atom: nobody's, and drawn as a dot rather than a symbol.
        let spot = valued(&entries, "b-m-p-s-m");
        assert_eq!(spot.path, ["Markers", "Spot marker"]);
        assert_eq!(spot.icon, None);
    }

    #[test]
    fn a_symbol_is_a_whole_code_in_the_edition_asked_for() {
        let entries = symbol_entries(
            &rows(&[(
                "10121100",
                &["Land unit", "Movement and Maneuver", "Infantry"],
            )]),
            Symbology::Milstd2525D,
            HOSTILE,
        );

        valued(&entries, "10061000001211000000");
    }
}
