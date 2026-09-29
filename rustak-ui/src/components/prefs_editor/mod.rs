//! The typed preference list a profile delivers.
//!
//! Three things make this more than a key/value table.
//!
//! **Known keys are drawn from the catalogue.** The server describes the ATAK
//! preferences it can vouch for as a JSON Schema
//! ([`rustak_api::pref_catalog`]), and a catalogued key is chosen from a
//! searchable picker and edited with the control its schema calls for — the
//! same [`SchemaNode`](super::SchemaNode) that draws a service's
//! configuration. Its Java class comes with it: ATAK's importer dereferences
//! the `class` attribute of every entry without checking whether it is there,
//! and it is the one thing an operator cannot be expected to know.
//!
//! **Any other key is still possible.** ATAK's plugins define keys no catalogue
//! can know, so an "other" row keeps the key, the class and the value free —
//! and a stored entry the catalogue disagrees with is drawn as one too, with a
//! note, rather than being converted behind anybody's back. See
//! [`model`] for exactly when a row is which.
//!
//! **The order is the delivery order.** The `.pref` document renders entries in
//! the order they are stored, so two builds of one profile produce the same
//! bytes; the editor therefore appends rather than sorting.

mod model;
mod rows;

use rustak_api::PrefEntry;
use serde_json::Value;
use yew::prelude::*;

use super::{Button, ButtonKind, Field, Tree, TreeSelect};
use model::{Catalog, Row};
use rows::{KnownRow, OtherRow};

/// Why a list could not be saved, or `None` when it could.
///
/// The same three rules the server applies, so the button is disabled for
/// exactly the reasons a request would be refused.
pub fn problem_with(entries: &[PrefEntry]) -> Option<String> {
    for (index, entry) in entries.iter().enumerate() {
        if entry.key.trim().is_empty() {
            return Some(format!("Preference {} has no key.", index + 1));
        }

        if entries
            .iter()
            .take(index)
            .any(|earlier| earlier.key.trim() == entry.key.trim())
        {
            return Some(format!("'{}' is listed twice.", entry.key.trim()));
        }

        if !entry.class.accepts(&entry.value) {
            return Some(format!(
                "'{}' is not a {} value for '{}'.",
                entry.value,
                entry.class.as_str(),
                entry.key.trim(),
            ));
        }
    }

    None
}

#[derive(Properties, PartialEq)]
pub struct PrefsEditorProps {
    /// The list as it currently stands.
    pub value: Vec<PrefEntry>,

    /// The whole list after any change, ready to be `PUT` back.
    pub onchange: Callback<Vec<PrefEntry>>,

    /// The catalogue's JSON Schema, as the server serves it. Without one,
    /// every row is an "other" row, which is still a complete editor.
    #[prop_or_default]
    pub catalog: Option<Value>,

    #[prop_or_default]
    pub disabled: bool,
}

#[function_component(PrefsEditor)]
pub fn prefs_editor(props: &PrefsEditorProps) -> Html {
    let catalog = use_memo(props.catalog.clone(), |schema| {
        schema.clone().map(Catalog::new).unwrap_or_default()
    });
    let offered = use_memo((catalog.clone(), props.value.clone()), |(catalog, held)| {
        Tree::build(catalog.offered(held))
    });

    let append = {
        let (entries, onchange) = (props.value.clone(), props.onchange.clone());
        move |entry: PrefEntry| {
            let mut next = entries.clone();
            next.push(entry);
            onchange.emit(next);
        }
    };
    let add_known = {
        let (catalog, append) = (catalog.clone(), append.clone());
        Callback::from(move |key: Option<String>| {
            // A key typed rather than picked is one the catalogue does not
            // list, and becomes an "other" row holding it.
            let key = key.unwrap_or_default();
            append(
                catalog
                    .fresh(&key)
                    .unwrap_or_else(|| PrefEntry::string(key, "")),
            );
        })
    };
    let add_other = Callback::from(move |_: MouseEvent| append(PrefEntry::string("", "")));

    let typed = {
        let catalog = catalog.clone();
        let held = props.value.clone();
        Callback::from(move |text: String| {
            let text = text.trim().to_string();
            // Words with spaces between them are a search for a title, not a
            // key; a key that genuinely holds a space is an "other" row away.
            let free = !text.is_empty()
                && !text.contains(char::is_whitespace)
                && catalog.class(&text).is_none()
                && !held.iter().any(|entry| entry.key.trim() == text);
            free.then_some(text)
        })
    };

    html! {
        <div class="prefs-editor">
            if props.value.is_empty() {
                <p class="panel-empty">
                    { "No preferences yet. A profile with none delivers only its files." }
                </p>
            } else {
                <ul class="prefs-editor__list">
                    { for props.value.iter().enumerate().map(|(index, entry)| html! {
                        <li key={index}>{ row(props, &catalog, index, entry) }</li>
                    }) }
                </ul>
            }

            <div class="prefs-editor__add">
                <Field
                    label="Add a preference"
                    id="pref-add"
                    help="Search ATAK's preferences by name or key. A key the catalogue does not \
                          list can be typed in as it stands."
                >
                    <TreeSelect
                        id="pref-add"
                        tree={offered}
                        value={None::<AttrValue>}
                        onchange={add_known}
                        placeholder="Choose a preference"
                        disabled={props.disabled}
                        {typed}
                    />
                </Field>
                <Button small=true kind={ButtonKind::Default} disabled={props.disabled} onclick={add_other}>
                    { "Add another preference" }
                </Button>
            </div>
        </div>
    }
}

/// One entry, drawn as whichever kind of row the catalogue says it is.
fn row(props: &PrefsEditorProps, catalog: &Catalog, index: usize, entry: &PrefEntry) -> Html {
    let onchange = {
        let (entries, onchange) = (props.value.clone(), props.onchange.clone());
        Callback::from(move |changed: Option<PrefEntry>| {
            let mut next = entries.clone();
            match changed {
                Some(entry) => next[index] = entry,
                None => {
                    next.remove(index);
                }
            }
            onchange.emit(next);
        })
    };

    match catalog.row(entry) {
        Row::Known {
            property,
            reads,
            value,
            note,
        } => html! {
            <KnownRow
                {index}
                entry={entry.clone()}
                root={catalog.root()}
                {property}
                {reads}
                {value}
                {note}
                disabled={props.disabled}
                {onchange}
            />
        },
        Row::Other { note, adopt } => html! {
            <OtherRow
                {index}
                entry={entry.clone()}
                {note}
                {adopt}
                disabled={props.disabled}
                {onchange}
            />
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_api::PrefClass;

    fn entry(key: &str, class: PrefClass, value: &str) -> PrefEntry {
        PrefEntry::new(key, class, value)
    }

    #[test]
    fn a_list_the_server_would_accept_has_no_problem() {
        let entries = vec![
            entry("deviceProfileEnableOnConnect", PrefClass::Boolean, "true"),
            entry("dynamicReportingRateMinReliable", PrefClass::Integer, "20"),
        ];

        assert_eq!(problem_with(&entries), None);
    }

    #[test]
    fn a_blank_key_is_named_by_its_position() {
        let entries = vec![
            entry("a", PrefClass::String, "1"),
            entry("  ", PrefClass::String, "1"),
        ];

        assert_eq!(
            problem_with(&entries).as_deref(),
            Some("Preference 2 has no key."),
        );
    }

    #[test]
    fn the_same_key_twice_is_refused_because_the_second_would_win_silently() {
        let entries = vec![
            entry("locationTeam", PrefClass::String, "Cyan"),
            entry("locationTeam", PrefClass::String, "Green"),
        ];

        assert_eq!(
            problem_with(&entries).as_deref(),
            Some("'locationTeam' is listed twice."),
        );
    }

    #[test]
    fn a_value_its_class_could_not_hold_is_refused_with_both_named() {
        let entries = vec![entry("rate", PrefClass::Integer, "3.5")];

        assert_eq!(
            problem_with(&entries).as_deref(),
            Some("'3.5' is not a Integer value for 'rate'."),
        );
    }
}
