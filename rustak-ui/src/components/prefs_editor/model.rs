//! What the preference editor decides, with no browser in it.
//!
//! Which rows the catalogue can draw, what each is drawn with, and what is said
//! about a stored entry the catalogue disagrees with. All of it is a function
//! of the catalogue's schema and the stored list, so all of it is tested
//! natively.
//!
//! # A row is drawn from the catalogue only when that changes nothing
//!
//! A stored entry is drawn with its schema's control when the catalogue knows
//! its key **and** it is stored as one of the classes the key is known to be
//! sent as — a boolean-valued key stored as a String holding `"true"` is drawn
//! as a toggle, and edited it stays a String. The control follows what the key
//! *means* (the class ATAK reads it as); the class it is stored as is never
//! changed by editing, so an untouched profile is sent byte for byte as before.
//!
//! Anything else — a key nobody catalogued, one stored as a class nobody is
//! known to send it as, or a boolean-valued key holding something other than
//! `true`/`false` — is an "other" row, whose key, class and value are free
//! text, because redrawing it with the catalogue's control would mean
//! converting it, and converting it is the operator's call: a catalogued one
//! says why it disagrees and offers the conversion as a button. A catalogued
//! row whose *value* the catalogue does not list keeps that value, untouched,
//! with a note.

use std::rc::Rc;

use rustak_api::pref_catalog::{self, GROUP_KEYWORD};
use rustak_api::{PrefClass, PrefEntry};
use serde_json::{Value, json};

use crate::components::TreeEntry;
use crate::components::schema_form::schema::{self, Kind};

/// The catalogue's schema, as the editor reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog(Rc<Value>);

/// How one stored entry is drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// With its schema's control.
    Known {
        /// The property, with ATAK's default and an example folded into its
        /// description, which is what the form shows as help.
        property: Value,
        /// The class ATAK reads the key as, which is what the control edits.
        reads: PrefClass,
        /// The stored value as the control edits it.
        value: Option<Value>,
        /// What the catalogue says about a stored value it does not list.
        note: Option<String>,
    },
    /// As a free key, class and value.
    Other {
        /// Why a catalogued key is not drawn with its control.
        note: Option<String>,
        /// The conversion that would make it one: what the button says, and
        /// the entry it writes.
        adopt: Option<(String, PrefEntry)>,
    },
}

impl Catalog {
    pub fn new(schema: Value) -> Self {
        Self(Rc::new(schema))
    }

    pub fn root(&self) -> Rc<Value> {
        Rc::clone(&self.0)
    }

    fn property(&self, key: &str) -> Option<&Value> {
        self.0.get("properties")?.get(key.trim())
    }

    pub fn class(&self, key: &str) -> Option<PrefClass> {
        pref_catalog::class_of(&self.0, key.trim())
    }

    /// The catalogued keys a list does not already hold, for the picker: filed
    /// under their group, named by their title, searchable by their key.
    pub fn offered(&self, held: &[PrefEntry]) -> Vec<TreeEntry> {
        let Some(properties) = self.0.get("properties").and_then(Value::as_object) else {
            return Vec::new();
        };
        let text =
            |node: &Value, name: &str| node.get(name).and_then(Value::as_str).map(str::to_string);

        let mut offered: Vec<TreeEntry> = properties
            .iter()
            .filter(|(key, _)| !held.iter().any(|entry| entry.key.trim() == key.as_str()))
            .map(|(key, node)| TreeEntry {
                path: vec![
                    text(node, GROUP_KEYWORD).unwrap_or_else(|| "Other".to_string()),
                    text(node, "title").unwrap_or_else(|| key.clone()),
                ],
                value: Some(key.clone()),
                detail: Some(key.clone()),
                icon: None,
            })
            .collect();

        offered.sort_by(|a, b| a.path.cmp(&b.path));
        offered
    }

    fn reads(&self, key: &str) -> Option<PrefClass> {
        pref_catalog::reads_of(&self.0, key.trim())
    }

    /// A new entry for a catalogued key: the class new entries are sent as,
    /// and ATAK's own default — or the emptiest value of its kind — to start
    /// from.
    pub fn fresh(&self, key: &str) -> Option<PrefEntry> {
        let (property, class, reads) = (self.property(key)?, self.class(key)?, self.reads(key)?);
        let start = schema::default_for(&self.0, property);
        let value = pref_catalog::from_json(reads, &start).unwrap_or_default();

        Some(PrefEntry::new(key.trim(), class, value))
    }

    /// How `entry` is drawn.
    pub fn row(&self, entry: &PrefEntry) -> Row {
        let (Some(property), Some(reads)) = (self.property(&entry.key), self.reads(&entry.key))
        else {
            return Row::Other {
                note: None,
                adopt: None,
            };
        };
        let accepted = pref_catalog::classes_of(&self.0, entry.key.trim());
        let value = pref_catalog::to_json(reads, &entry.value);

        if !accepted.contains(&entry.class) {
            return self.unaccepted(entry, &accepted, reads);
        }

        // A switch can show nothing but on and off; any other kind of input can
        // show what is stored, and says what is wrong with it.
        if reads == PrefClass::Boolean && value.is_none() {
            let fresh = self.fresh(&entry.key).map(|fresh| PrefEntry {
                value: fresh.value,
                ..entry.clone()
            });
            return Row::Other {
                note: Some(format!(
                    "ATAK reads this key as a Boolean, which '{}' is not. It is sent as stored.",
                    entry.value,
                )),
                adopt: fresh.map(|fresh| (format!("Replace it with {}", fresh.value), fresh)),
            };
        }

        Row::Known {
            property: with_hints(&self.0, property),
            note: value_note(&self.0, property, entry, reads, value.as_ref()),
            reads,
            value,
        }
    }

    /// An entry stored as a class the key is not known to be sent as.
    fn unaccepted(&self, entry: &PrefEntry, accepted: &[PrefClass], reads: PrefClass) -> Row {
        let names: Vec<&str> = accepted.iter().map(PrefClass::as_str).collect();
        let adopt = accepted.first().map(|preferred| {
            let fits = pref_catalog::to_json(reads, &entry.value).is_some()
                && preferred.accepts(&entry.value);
            let adopted = match fits {
                true => PrefEntry {
                    class: *preferred,
                    ..entry.clone()
                },
                false => self.fresh(&entry.key).unwrap_or_else(|| PrefEntry {
                    class: *preferred,
                    ..entry.clone()
                }),
            };
            (format!("Send it as a {}", preferred.as_str()), adopted)
        });

        Row::Other {
            note: Some(format!(
                "The catalogue knows this key sent as a {}; this entry is sent as a {}, as it is \
                 stored.",
                names.join(" or a "),
                entry.class.as_str(),
            )),
            adopt,
        }
    }
}

/// `property`, with what its default and examples are said in its description.
fn with_hints(root: &Value, property: &Value) -> Value {
    let mut said = vec![
        property
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    ];

    if let Some(default) = property.get("default") {
        let shown = match default {
            Value::Bool(on) => if *on { "on" } else { "off" }.to_string(),
            other => schema::choice_label(root, property, other)
                .map(str::to_string)
                .or_else(|| other.as_str().map(str::to_string))
                .unwrap_or_else(|| other.to_string()),
        };
        said.push(format!("ATAK's own default is {shown}."));
    }
    if let Some(example) = property.pointer("/examples/0").and_then(Value::as_str) {
        said.push(format!("For example: {example}"));
    }

    let mut hinted = property.clone();
    hinted["description"] = json!(said.join(" ").trim());
    hinted
}

/// What to say about a stored value the catalogue does not list.
fn value_note(
    root: &Value,
    property: &Value,
    entry: &PrefEntry,
    reads: PrefClass,
    value: Option<&Value>,
) -> Option<String> {
    let Some(value) = value else {
        return Some(format!(
            "ATAK reads this key as a {}, which '{}' is not.",
            reads.as_str(),
            entry.value,
        ));
    };

    let outside = match schema::kind(root, property) {
        Kind::Choice(listed) => !listed.contains(&value),
        Kind::Integer | Kind::Number => {
            let (number, bound) = (value.as_f64(), |name| {
                property.get(name).and_then(Value::as_f64)
            });
            number.is_some_and(|n| bound("minimum").is_some_and(|min| n < min))
                || number.is_some_and(|n| bound("maximum").is_some_and(|max| n > max))
        }
        _ => false,
    };

    outside.then(|| {
        format!(
            "'{}' is not a value the catalogue lists for this preference. It is kept, and sent, as \
             stored until you choose another.",
            entry.value,
        )
    })
}

/// `entry` after its control reported `value`, for a key ATAK reads as
/// `reads`. The class it is stored as is kept: editing a String entry for a
/// boolean-valued key writes `"true"` or `"false"` into a String.
pub fn edited(entry: &PrefEntry, reads: PrefClass, value: Option<Value>) -> PrefEntry {
    PrefEntry {
        value: text_of(reads, value),
        ..entry.clone()
    }
}

/// What a control reported, as the text an entry of `class` carries. A value
/// the class cannot hold is kept as typed, so that the list says what is wrong
/// with it rather than quietly dropping it.
pub fn text_of(class: PrefClass, value: Option<Value>) -> String {
    match value {
        None => String::new(),
        Some(value) => pref_catalog::from_json(class, &value).unwrap_or_else(|| match value {
            Value::String(text) => text,
            other => other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::new(pref_catalog::schema())
    }

    fn known(entry: &PrefEntry) -> (Value, PrefClass, Option<Value>, Option<String>) {
        match catalog().row(entry) {
            Row::Known {
                property,
                reads,
                value,
                note,
            } => (property, reads, value, note),
            other => panic!("{entry:?} is drawn as {other:?}"),
        }
    }

    #[test]
    fn a_catalogued_key_stored_as_its_class_is_drawn_with_its_control() {
        let entry = PrefEntry::new("deviceProfileEnableOnConnect", PrefClass::Boolean, "true");

        let (property, reads, value, note) = known(&entry);
        assert_eq!(
            (reads, value, note),
            (PrefClass::Boolean, Some(json!(true)), None)
        );
        assert!(
            property["description"]
                .as_str()
                .unwrap()
                .ends_with("ATAK's own default is off."),
            "{property}",
        );
    }

    #[test]
    fn a_boolean_key_sent_as_a_string_is_a_toggle_that_stays_a_string() {
        // What this server's own enrolment profile, TAK Server and
        // OpenTAKServer send: a String holding "true".
        let entry = PrefEntry::string("prefs_enable_channels", "true");

        let (property, reads, value, note) = known(&entry);
        assert_eq!(reads, PrefClass::Boolean);
        assert_eq!(value, Some(json!(true)), "drawn as a switch");
        assert_eq!(note, None, "not a disagreement");
        assert_eq!(property["type"], "boolean");

        // Untouched, it is written back exactly as stored.
        assert_eq!(edited(&entry, reads, value), entry);

        // Toggled, it is still a String, now holding "false".
        assert_eq!(
            edited(&entry, reads, Some(json!(false))),
            PrefEntry::string("prefs_enable_channels", "false"),
        );
    }

    #[test]
    fn a_class_nobody_sends_the_key_as_is_an_other_row_offering_the_preferred_one() {
        let entry = PrefEntry::new("dispatchLocationHidden", PrefClass::Integer, "1");

        let Row::Other { note, adopt } = catalog().row(&entry) else {
            panic!("an Integer is not how this key is sent");
        };
        assert!(
            note.unwrap()
                .contains("sent as a Boolean; this entry is sent as a Integer")
        );
        let (label, adopted) = adopt.unwrap();
        assert_eq!(label, "Send it as a Boolean");
        assert_eq!(adopted.class, PrefClass::Boolean);
        assert_eq!(
            adopted.value, "false",
            "'1' is not a boolean, so it starts again"
        );

        assert_eq!(
            catalog().row(&PrefEntry::string("com.example.flag", "1")),
            Row::Other {
                note: None,
                adopt: None
            },
        );
    }

    #[test]
    fn a_boolean_key_holding_something_else_is_an_other_row_kept_as_stored() {
        let entry = PrefEntry::string("prefs_enable_channels", "yes");

        let Row::Other { note, adopt } = catalog().row(&entry) else {
            panic!("a switch cannot show 'yes'");
        };
        assert!(note.unwrap().contains("which 'yes' is not"));
        assert_eq!(
            adopt.map(|(_, entry)| entry),
            Some(PrefEntry::string("prefs_enable_channels", "false")),
            "offered as the default, in the class it is stored as",
        );
    }

    #[test]
    fn a_value_the_catalogue_does_not_list_is_kept_with_a_note() {
        let entry = PrefEntry::string("locationReportingStrategy", "Sometimes");

        let (property, _, value, note) = known(&entry);
        assert_eq!(value, Some(json!("Sometimes")), "kept as stored");
        assert!(
            note.unwrap()
                .contains("'Sometimes' is not a value the catalogue lists")
        );
        assert!(
            property["description"]
                .as_str()
                .unwrap()
                .contains("ATAK's own default is Dynamic.")
        );
    }

    #[test]
    fn a_number_outside_its_bounds_is_noted_and_one_inside_them_is_not() {
        let root = json!({ "properties": { "rate": {
            "type": "integer", "x-atak-reads": "Integer", "x-atak-classes": ["Integer"],
            "minimum": 1, "maximum": 60,
        } } });
        let catalog = Catalog::new(root);

        for (raw, noted) in [("30", false), ("0", true), ("61", true), ("", true)] {
            let Row::Known { note, .. } =
                catalog.row(&PrefEntry::new("rate", PrefClass::Integer, raw))
            else {
                panic!("an Integer key stored as an Integer");
            };
            assert_eq!(note.is_some(), noted, "{raw}");
        }
    }

    #[test]
    fn a_new_entry_takes_the_class_new_entries_are_sent_as_and_its_default() {
        let catalog = catalog();

        assert_eq!(
            catalog.fresh("locationReportingStrategy"),
            Some(PrefEntry::string("locationReportingStrategy", "Dynamic")),
        );
        assert_eq!(
            catalog.fresh("deviceProfileEnableOnConnect"),
            Some(PrefEntry::string("deviceProfileEnableOnConnect", "false")),
            "a String, as this server's enrolment profile sends it",
        );
        assert_eq!(
            catalog.fresh("repoStartupSync"),
            Some(PrefEntry::new(
                "repoStartupSync",
                PrefClass::Boolean,
                "false"
            )),
        );
        assert_eq!(
            catalog.fresh("locationCallsign"),
            Some(PrefEntry::string("locationCallsign", "")),
        );
        assert_eq!(catalog.fresh("com.example.flag"), None);

        let all = catalog.offered(&[]).len();
        let held = [PrefEntry::string("locationCallsign", "Alpha")];
        let offered = catalog.offered(&held);
        assert_eq!(offered.len(), all - 1);
        assert!(
            offered
                .iter()
                .all(|entry| entry.value.as_deref() != Some("locationCallsign"))
        );
        assert!(
            offered.iter().all(|entry| entry.path.len() == 2),
            "group, then title"
        );
    }

    #[test]
    fn what_a_control_reports_becomes_the_text_its_class_carries() {
        assert_eq!(text_of(PrefClass::Boolean, Some(json!(false))), "false");
        assert_eq!(text_of(PrefClass::Integer, Some(json!(20))), "20");
        assert_eq!(text_of(PrefClass::String, Some(json!("Cyan"))), "Cyan");
        assert_eq!(text_of(PrefClass::Integer, None), "");
        assert_eq!(
            text_of(PrefClass::Integer, Some(json!(9_000_000_000_i64))),
            "9000000000",
            "kept as typed, for the list to refuse by name",
        );
    }
}
