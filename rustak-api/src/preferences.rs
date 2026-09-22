//! What one person has chosen about how this console looks to them.
//!
//! Distinct from [`settings`](crate::settings), which are the installation's
//! and an administrator's to change, and from a device profile's preferences,
//! which are ATAK's. These belong to an account, follow it to whichever browser
//! it signs in from, and are nobody else's business: anybody may read and
//! change their own, and nobody may read or change another's.
//!
//! # Every field has a default, and an absent one means the default
//!
//! A preference nobody has set is not stored, so the type on the wire is a
//! struct of plain values rather than of options: a page never has to decide
//! what "unset" looks like. The patch is the other way round — every field
//! optional — so that changing one thing cannot reset another that a newer
//! build of the page knows about and this one does not.

use serde::{Deserialize, Serialize};

/// One account's preferences, complete: what was chosen, and the default for
/// what was not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreferences {
    /// Which edition of MIL-STD-2525 the map draws its symbols from.
    #[serde(default)]
    pub symbology: Symbology,
}

/// A change to one account's preferences. What is absent is left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreferencesPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbology: Option<Symbology>,
}

impl UserPreferencesPatch {
    /// Whether applying this would change nothing.
    pub fn is_empty(&self) -> bool {
        self.symbology.is_none()
    }
}

impl UserPreferences {
    /// What would have to be sent to turn these preferences into `next`: only
    /// the fields that differ, which is what `PATCH /me/preferences` takes.
    #[must_use]
    pub fn changes_to(&self, next: &Self) -> UserPreferencesPatch {
        UserPreferencesPatch {
            symbology: Some(next.symbology).filter(|symbology| *symbology != self.symbology),
        }
    }
}

/// A JSON Schema for [`UserPreferences`], which is what the console draws the
/// account's preferences from — with the same form it draws a service's
/// configuration from.
///
/// Written out rather than derived, because this crate is compiled to wasm for
/// the console and carries no schema library; this module's tests hold it to the type, so
/// an edition added to [`Symbology`] and not offered here is a failing test
/// rather than a choice nobody can make. Every property is required: a
/// preference always has a value, even when it is only the default.
#[must_use]
pub fn schema() -> serde_json::Value {
    let editions: Vec<serde_json::Value> = Symbology::ALL
        .into_iter()
        .map(|edition| serde_json::json!({ "const": edition.as_str(), "title": edition.label() }))
        .collect();

    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Preferences",
        "type": "object",
        "required": ["symbology"],
        "properties": {
            "symbology": {
                "title": "Symbol edition",
                "description": "Which edition of MIL-STD-2525 your devices author symbols in. It \
                    reaches each device with its profile, the next time it connects. The map here \
                    draws every symbol in the edition its sender wrote it in.",
                "default": Symbology::default().as_str(),
                "oneOf": editions,
            },
        },
    })
}

/// An edition of MIL-STD-2525, the US standard for military map symbols.
///
/// CoT types were laid out on 2525B/C's hierarchy, which is why C is the
/// default: every atom type has a C symbol by construction. D (2014) redrew
/// much of the iconography and re-coded every symbol as twenty digits; it is
/// what newer TAK clients can be switched to, and somebody who works in one
/// should not have to read the other here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Symbology {
    #[default]
    #[serde(rename = "2525c")]
    Milstd2525C,

    #[serde(rename = "2525d")]
    Milstd2525D,
}

impl Symbology {
    pub const ALL: [Self; 2] = [Self::Milstd2525C, Self::Milstd2525D];

    /// The value as it is stored and sent.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Milstd2525C => "2525c",
            Self::Milstd2525D => "2525d",
        }
    }

    /// Reads a stored value. [`None`] for anything this build has not heard of,
    /// which a caller treats as "not set" rather than as an error: a row written
    /// by a newer server must not stop an older one from answering `/me`.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str() == value)
    }

    /// What to call it in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Milstd2525C => "MIL-STD-2525C",
            Self::Milstd2525D => "MIL-STD-2525D",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_chosen_is_the_default_and_reads_back_whole() {
        let read: UserPreferences = serde_json::from_str("{}").unwrap();

        assert_eq!(read, UserPreferences::default());
        assert_eq!(read.symbology, Symbology::Milstd2525C);
        assert_eq!(
            serde_json::to_value(read).unwrap(),
            serde_json::json!({ "symbology": "2525c" })
        );
    }

    #[test]
    fn every_edition_is_stored_as_it_is_sent() {
        for edition in Symbology::ALL {
            assert_eq!(
                serde_json::to_value(edition).unwrap(),
                serde_json::json!(edition.as_str())
            );
            assert_eq!(Symbology::parse(edition.as_str()), Some(edition));
        }

        assert_eq!(Symbology::parse("app6d"), None);
    }

    #[test]
    fn a_patch_says_only_what_it_changes() {
        let nothing = UserPreferencesPatch::default();
        assert!(nothing.is_empty());
        assert_eq!(serde_json::to_string(&nothing).unwrap(), "{}");

        let one: UserPreferencesPatch = serde_json::from_str(r#"{"symbology":"2525d"}"#).unwrap();
        assert_eq!(one.symbology, Some(Symbology::Milstd2525D));
        assert!(!one.is_empty());
    }

    #[test]
    fn the_schema_describes_exactly_what_the_type_holds() {
        let schema = schema();
        let held = serde_json::to_value(UserPreferences::default()).unwrap();

        // Every field the type serialises is a required property, and no more.
        let mut described: Vec<&String> =
            schema["properties"].as_object().unwrap().keys().collect();
        let mut fields: Vec<&String> = held.as_object().unwrap().keys().collect();
        described.sort();
        fields.sort();
        assert_eq!(described, fields);
        assert_eq!(schema["required"].as_array().unwrap().len(), fields.len());

        // Every edition is offered under the value it is stored as.
        let offered: Vec<&str> = schema["properties"]["symbology"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["const"].as_str())
            .collect();
        let known: Vec<&str> = Symbology::ALL.into_iter().map(Symbology::as_str).collect();
        assert_eq!(offered, known);
        assert_eq!(
            schema["properties"]["symbology"]["default"],
            held["symbology"]
        );
    }

    #[test]
    fn only_what_differs_is_a_change() {
        let current = UserPreferences::default();
        let next = UserPreferences {
            symbology: Symbology::Milstd2525D,
        };

        assert!(current.changes_to(&current).is_empty());
        assert_eq!(
            current.changes_to(&next).symbology,
            Some(Symbology::Milstd2525D)
        );
    }
}
