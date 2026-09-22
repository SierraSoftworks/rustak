//! A CoT type as a MIL-STD-2525 symbol identification code.
//!
//! CoT's atom types were laid out on the 2525 hierarchy on purpose:
//! `a-f-G-U-C` is an atom (`a`) that is friendly (`f`), on the ground (`G`),
//! and a combat unit (`U-C`). 2525C's letter code spells the same thing as
//! `SFGPUC---------`: a warfighting symbol (`S`), friendly (`F`), ground (`G`),
//! present (`P`), function `UC`, padded to fifteen. So the mapping is a
//! rearrangement rather than a table, and it is written here from the two
//! public formats rather than taken from anybody's implementation of it.
//!
//! milsymbol draws the code. A function it has no icon for still gets a frame
//! — the shape says the battle dimension and the colour says whose it is —
//! which is the right answer for the long tail of types nobody standardised.

/// Whose something is, as CoT spells it and as anybody would say it.
const AFFILIATIONS: &[(&str, char, &str)] = &[
    ("p", 'P', "Pending"),
    ("u", 'U', "Unknown"),
    ("a", 'A', "Assumed friend"),
    ("f", 'F', "Friendly"),
    ("n", 'N', "Neutral"),
    ("s", 'S', "Suspect"),
    ("h", 'H', "Hostile"),
    ("j", 'J', "Joker"),
    ("k", 'K', "Faker"),
    // "None specified" and "other" have no frame of their own in 2525.
    ("o", 'U', "Unspecified"),
    ("x", 'U', "Other"),
];

/// Where something operates: the letter both formats use, and the word.
const DIMENSIONS: &[(&str, &str)] = &[
    ("P", "Space"),
    ("A", "Air"),
    ("G", "Ground"),
    ("S", "Sea surface"),
    ("U", "Subsurface"),
    ("F", "Special operations"),
    ("X", "Other"),
];

/// The 2525C code for an atom, or [`None`] for anything that is not one.
///
/// A type that stops at its affiliation (`a-h`) is still a thing on a map, so
/// it is drawn as a ground track of that affiliation rather than not at all.
pub fn from_cot_type(kind: &str) -> Option<String> {
    let mut parts = kind.split('-');
    if parts.next() != Some("a") {
        return None;
    }

    let affiliation = parts.next()?;
    let (_, affiliation, _) = AFFILIATIONS
        .iter()
        .find(|(cot, _, _)| *cot == affiliation)?;

    let dimension = parts
        .next()
        .filter(|given| DIMENSIONS.iter().any(|(letter, _)| letter == given))
        .unwrap_or("G");

    // Whatever is left is the function. CoT spells it one letter to a segment;
    // 2525C gives it six columns, and a type more specific than that is drawn
    // as the most specific thing the code can say.
    let function: String = parts
        .flat_map(str::chars)
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_uppercase())
        .take(6)
        .collect();

    Some(format!("S{affiliation}{dimension}P{function:-<6}-----"))
}

/// `Friendly · Ground`, for a pop-over. [`None`] for anything but an atom.
pub fn describe(kind: &str) -> Option<String> {
    let mut parts = kind.split('-').skip(1);

    let affiliation = parts.next()?;
    let (_, _, affiliation) = AFFILIATIONS
        .iter()
        .find(|(cot, _, _)| *cot == affiliation)?;

    let dimension = parts
        .next()
        .and_then(|given| DIMENSIONS.iter().find(|(letter, _)| *letter == given));

    kind.starts_with("a-").then(|| match dimension {
        Some((_, dimension)) => format!("{affiliation} · {dimension}"),
        None => (*affiliation).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_atom_is_rearranged_into_the_fifteen_columns_2525_gives_it() {
        for (kind, sidc) in [
            ("a-f-G-U-C", "SFGPUC---------"),
            ("a-h-A-M-F-Q", "SHAPMFQ--------"),
            ("a-n-S-X-M", "SNSPXM---------"),
            ("a-u-G", "SUGP-----------"),
            // None-specified has no frame of its own, so it borrows unknown's.
            ("a-o-G", "SUGP-----------"),
            // Deeper than six columns: as specific as the code can be.
            ("a-f-G-E-V-A-T-H-X", "SFGPEVATHX-----"),
            // No battle dimension at all is still something on the ground.
            ("a-h", "SHGP-----------"),
        ] {
            assert_eq!(from_cot_type(kind).as_deref(), Some(sidc), "{kind}");
        }
    }

    #[test]
    fn every_code_is_fifteen_columns() {
        for kind in ["a-f-G", "a-f-G-U-C-I", "a-f-G-E-V-A-T-H-X-Y-Z"] {
            assert_eq!(from_cot_type(kind).unwrap().len(), 15, "{kind}");
        }
    }

    #[test]
    fn only_an_atom_with_a_known_affiliation_has_a_symbol() {
        for kind in ["b-m-p-s-m", "u-d-f", "t-x-c-t", "a", "a-?-G", ""] {
            assert_eq!(from_cot_type(kind), None, "{kind}");
        }
    }

    #[test]
    fn a_type_is_described_in_the_words_somebody_would_use() {
        assert_eq!(describe("a-f-G-U-C").as_deref(), Some("Friendly · Ground"));
        assert_eq!(describe("a-h-A").as_deref(), Some("Hostile · Air"));
        assert_eq!(describe("a-s").as_deref(), Some("Suspect"));
        assert_eq!(describe("b-m-p-s-m"), None);
    }
}
