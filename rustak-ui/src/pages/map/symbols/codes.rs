//! The arithmetic between a CoT type, a MIL-STD-2525C letter code, a 2525D
//! number code, and the short codes the symbol catalogues are keyed by.
//!
//! The catalogues (`/vendor/symbology/*.json`, derived by `scripts/vendor.mjs`)
//! list every symbol without saying whose it is, because that is a separate
//! choice: a 2525C row is a battle dimension and a function (`GUCI---`), and a
//! 2525D row is a symbol set and an entity (`10121100`). Whose it is comes
//! from the affiliation somebody picks beside it.
//!
//! A CoT atom is the same 2525C row spelled with hyphens — `a-f-G-U-C-I` — so
//! one catalogue serves both the type and the 2525C symbol. See
//! [`crate::util::sidc`] for the same fact read in the other direction.

use rustak_api::Symbology;

/// Whose something is: CoT's letter, 2525C's, 2525D's digit, and the word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Affiliation {
    pub cot: &'static str,
    pub letter: char,
    pub identity: char,
    pub label: &'static str,
}

const fn affiliation(
    cot: &'static str,
    letter: char,
    identity: char,
    label: &'static str,
) -> Affiliation {
    Affiliation {
        cot,
        letter,
        identity,
        label,
    }
}

/// The affiliations somebody is offered, in the order 2525 lists them.
pub const AFFILIATIONS: [Affiliation; 9] = [
    affiliation("p", 'P', '0', "Pending"),
    affiliation("u", 'U', '1', "Unknown"),
    affiliation("a", 'A', '2', "Assumed friend"),
    affiliation("f", 'F', '3', "Friendly"),
    affiliation("n", 'N', '4', "Neutral"),
    affiliation("s", 'S', '5', "Suspect"),
    affiliation("h", 'H', '6', "Hostile"),
    // 2525D folds the exercise pair into suspect and hostile.
    affiliation("j", 'J', '5', "Joker"),
    affiliation("k", 'K', '6', "Faker"),
];

/// What a marker is until somebody says whose it is.
pub const UNKNOWN: Affiliation = AFFILIATIONS[1];

/// The affiliation a CoT type names in so many words. [`None`] for anything
/// that is not an atom, and for CoT's own "none specified" (`a-o`) and "other"
/// (`a-x`), which are atoms that say nothing about whose they are.
pub fn frame_of(kind: &str) -> Option<Affiliation> {
    let mut parts = kind.split('-');

    match (parts.next(), parts.next()) {
        (Some("a"), Some(cot)) => AFFILIATIONS.into_iter().find(|known| known.cot == cot),
        _ => None,
    }
}

/// The affiliation something is *drawn* as: the one its type names, and
/// unknown for a type that names none, which is the frame 2525 gives it.
pub fn affiliation_of(kind: &str) -> Affiliation {
    frame_of(kind).unwrap_or(UNKNOWN)
}

/// Whether a type is an atom, which is the only kind that has an affiliation.
pub fn is_atom(kind: &str) -> bool {
    kind.starts_with("a-")
}

/// The CoT type for a 2525C catalogue row: `GUCI---` as `a-f-G-U-C-I`.
pub fn cot_type(whose: Affiliation, row: &str) -> String {
    row.chars()
        .filter(|column| *column != '-')
        .fold(format!("a-{}", whose.cot), |kind, column| {
            format!("{kind}-{column}")
        })
}

/// The same type as somebody else's: `a-f-G-U-C` as `a-h-G-U-C`. Anything that
/// is not an atom is left as it is.
pub fn retyped(kind: &str, whose: Affiliation) -> String {
    let mut parts: Vec<&str> = kind.split('-').collect();

    match parts.len() >= 2 && parts[0] == "a" {
        true => {
            parts[1] = whose.cot;
            parts.join("-")
        }
        false => kind.to_string(),
    }
}

/// The symbol identification code for a catalogue row of `edition`.
pub fn sidc(edition: Symbology, whose: Affiliation, row: &str) -> String {
    match edition {
        Symbology::Milstd2525C => {
            let (dimension, function) = row.split_at(row.len().min(1));
            format!("S{}{dimension}P{function:-<6}-----", whose.letter)
        }
        Symbology::Milstd2525D => {
            let (set, entity) = row.split_at(row.len().min(2));
            format!("100{}{set}0000{entity:0<6}0000", whose.identity)
        }
    }
}

/// The same symbol as somebody else's, in whichever edition it is written.
/// Anything that is not a code is left as it is.
///
/// 2525E's thirty digits open with the same ten as 2525D's twenty — version,
/// context, standard identity — so a code in either keeps step with the type
/// even though only 2525D has a catalogue here.
pub fn resided(code: &str, whose: Affiliation) -> String {
    let replaced = |at: usize, with: char| {
        code.chars()
            .enumerate()
            .map(|(index, column)| if index == at { with } else { column })
            .collect()
    };
    let digits = code.chars().all(|digit| digit.is_ascii_digit());

    match code.len() {
        15 if code.is_ascii() => replaced(1, whose.letter),
        20 | 30 if digits => replaced(3, whose.identity),
        _ => code.to_string(),
    }
}

/// Which edition's catalogue a code belongs in, going by its shape. [`None`]
/// for a 2525E code as much as for nonsense: it is a code, and is kept and
/// drawn as one, but there is no list here to find it in.
pub fn edition_of(code: &str) -> Option<Symbology> {
    match code.len() {
        15 if code.is_ascii() => Some(Symbology::Milstd2525C),
        20 if code.chars().all(|digit| digit.is_ascii_digit()) => Some(Symbology::Milstd2525D),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRIENDLY: Affiliation = AFFILIATIONS[3];
    const HOSTILE: Affiliation = AFFILIATIONS[6];

    #[test]
    fn a_catalogue_row_is_a_type_and_a_code_in_either_edition() {
        assert_eq!(cot_type(FRIENDLY, "GUCI---"), "a-f-G-U-C-I");
        assert_eq!(cot_type(HOSTILE, "A------"), "a-h-A");

        assert_eq!(
            sidc(Symbology::Milstd2525C, FRIENDLY, "GUCI---"),
            "SFGPUCI--------"
        );
        assert_eq!(
            sidc(Symbology::Milstd2525D, HOSTILE, "10121100"),
            "10061000001211000000"
        );

        // What is built here is what the rest of the console accepts and draws.
        for (edition, row) in [
            (Symbology::Milstd2525C, "GUCI---"),
            (Symbology::Milstd2525D, "10121100"),
        ] {
            let code = sidc(edition, FRIENDLY, row);

            assert_eq!(rustak_api::map::sidc(&code).as_deref(), Some(code.as_str()));
            assert_eq!(edition_of(&code), Some(edition));
        }
        assert_eq!(
            crate::util::sidc::from_cot_type(&cot_type(FRIENDLY, "GUCI---")).as_deref(),
            Some("SFGPUCI--------")
        );
    }

    #[test]
    fn whose_it_is_can_change_without_changing_what_it_is() {
        for (kind, expected) in [
            ("a-f-G-U-C", "a-h-G-U-C"),
            ("a-u", "a-h"),
            ("b-m-p-s-m", "b-m-p-s-m"),
        ] {
            assert_eq!(retyped(kind, HOSTILE), expected);
        }

        for (code, expected) in [
            ("SFGPUCI--------", "SHGPUCI--------"),
            ("10031000001211000000", "10061000001211000000"),
            // 2525E: no catalogue here, and still somebody's.
            (
                "130310000012110000000000000000",
                "130610000012110000000000000000",
            ),
            ("not a code", "not a code"),
        ] {
            assert_eq!(resided(code, HOSTILE), expected);
        }
    }

    #[test]
    fn only_an_atom_says_whose_it_is() {
        for (kind, expected) in [
            ("a-h-G", HOSTILE),
            ("a-f-A-M-H", FRIENDLY),
            // CoT's "none specified" has no frame of its own.
            ("a-o-G", UNKNOWN),
            ("b-m-p-s-m", UNKNOWN),
            ("", UNKNOWN),
        ] {
            assert_eq!(affiliation_of(kind), expected, "{kind:?}");
        }

        assert!(is_atom("a-u-G") && !is_atom("b-m-p-w"));

        // An atom that names nobody is drawn as unknown without claiming to be.
        assert_eq!(frame_of("a-h-G"), Some(HOSTILE));
        assert_eq!(frame_of("a-o-G"), None);
        assert_eq!(frame_of("b-m-p-s-m"), None);
        assert_eq!(edition_of("130310000012110000000000000000"), None);
    }
}
