//! The typed preference list a profile delivers.
//!
//! Three things make this more than a key/value table.
//!
//! **Every entry carries its Java class.** ATAK's importer dereferences the
//! `class` attribute of each entry without checking whether it is there, so an
//! entry whose type we have forgotten aborts the import of the *whole
//! document* rather than skipping one key. The class is therefore a field on
//! the row and not a guess made at render time.
//!
//! **A value is checked against its class here.** `PrefClass::accepts` is the
//! same predicate the server validates with, so a typo is caught while it is
//! still being typed rather than as a `400` after Save.
//!
//! **The order is the delivery order.** The `.pref` document renders entries in
//! the order they are stored, so two builds of one profile produce the same
//! bytes; the editor therefore appends rather than sorting, and moving a row
//! is an explicit action.

use rustak_api::{PrefCatalogEntry, PrefClass, PrefEntry};
use yew::prelude::*;

use crate::components::{Button, ButtonKind, Select, SelectOption, TextInput};

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

fn class_options() -> Vec<SelectOption> {
    PrefClass::ALL
        .iter()
        .map(|class| SelectOption::new(class.as_str(), class.as_str()))
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct PrefsEditorProps {
    /// The list as it currently stands.
    pub value: Vec<PrefEntry>,

    /// The whole list after any change, ready to be `PUT` back.
    pub onchange: Callback<Vec<PrefEntry>>,

    /// The keys worth suggesting, with the class and default each one has.
    #[prop_or_default]
    pub catalog: Vec<PrefCatalogEntry>,

    #[prop_or_default]
    pub disabled: bool,
}

#[function_component(PrefsEditor)]
pub fn prefs_editor(props: &PrefsEditorProps) -> Html {
    let list_id = "pref-catalog-keys";

    let replace = {
        let onchange = props.onchange.clone();
        move |entries: Vec<PrefEntry>| onchange.emit(entries)
    };

    let add = {
        let (entries, replace) = (props.value.clone(), replace.clone());
        Callback::from(move |_: MouseEvent| {
            let mut next = entries.clone();
            next.push(PrefEntry::string("", ""));
            replace(next);
        })
    };

    html! {
        <div class="prefs-editor">
            <datalist id={list_id}>
                { for props.catalog.iter().map(|entry| html! {
                    <option key={entry.key.clone()} value={entry.key.clone()} />
                }) }
            </datalist>

            if props.value.is_empty() {
                <p class="panel-empty">
                    { "No preferences yet. A profile with none delivers only its files." }
                </p>
            } else {
                <ul class="prefs-editor__list">
                    { for props.value.iter().enumerate().map(|(index, entry)| html! {
                        <li key={index}>
                            <PrefRow
                                index={index}
                                entry={entry.clone()}
                                catalog={props.catalog.clone()}
                                list={list_id}
                                disabled={props.disabled}
                                onchange={
                                    let (entries, replace) = (props.value.clone(), replace.clone());
                                    Callback::from(move |changed: Option<PrefEntry>| {
                                        let mut next = entries.clone();
                                        match changed {
                                            Some(entry) => next[index] = entry,
                                            None => { next.remove(index); }
                                        }
                                        replace(next);
                                    })
                                }
                            />
                        </li>
                    }) }
                </ul>
            }

            <Button
                small=true
                kind={ButtonKind::Default}
                disabled={props.disabled}
                onclick={add}
            >
                { "Add a preference" }
            </Button>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct PrefRowProps {
    index: usize,
    entry: PrefEntry,
    catalog: Vec<PrefCatalogEntry>,
    list: AttrValue,
    disabled: bool,
    /// The row after a change, or `None` when it is to be removed.
    onchange: Callback<Option<PrefEntry>>,
}

#[function_component(PrefRow)]
fn pref_row(props: &PrefRowProps) -> Html {
    let known = props
        .catalog
        .iter()
        .find(|known| known.key == props.entry.key.trim());

    let on_key = {
        let (entry, catalog, onchange) = (
            props.entry.clone(),
            props.catalog.clone(),
            props.onchange.clone(),
        );
        Callback::from(move |key: String| {
            let mut next = entry.clone();
            next.key = key;

            // Choosing a catalogue key brings its class with it, because the
            // class is a fact about the preference rather than a choice — and
            // the one thing a person cannot be expected to know is which of
            // ATAK's five Java types a key happens to be.
            if let Some(known) = catalog.iter().find(|known| known.key == next.key.trim()) {
                next.class = known.class;
            }

            onchange.emit(Some(next));
        })
    };

    let on_class = {
        let (entry, onchange) = (props.entry.clone(), props.onchange.clone());
        Callback::from(move |chosen: Option<String>| {
            let Some(class) = chosen.as_deref().and_then(PrefClass::parse) else {
                return;
            };

            onchange.emit(Some(PrefEntry {
                class,
                ..entry.clone()
            }));
        })
    };

    let on_value = {
        let (entry, onchange) = (props.entry.clone(), props.onchange.clone());
        Callback::from(move |value: String| {
            onchange.emit(Some(PrefEntry {
                value,
                ..entry.clone()
            }))
        })
    };

    let on_remove = {
        let onchange = props.onchange.clone();
        Callback::from(move |_: MouseEvent| onchange.emit(None))
    };

    let rejected = !props.entry.class.accepts(&props.entry.value);
    let id = |part: &str| format!("pref-{}-{part}", props.index);

    html! {
        <div class="pref-row">
            <TextInput
                id={id("key")}
                value={props.entry.key.clone()}
                list={props.list.clone()}
                placeholder="deviceProfileEnableOnConnect"
                disabled={props.disabled}
                invalid={props.entry.key.trim().is_empty()}
                onchange={on_key}
            />

            <Select
                id={id("class")}
                value={Some(AttrValue::from(props.entry.class.as_str()))}
                options={class_options()}
                disabled={props.disabled}
                onchange={on_class}
            />

            <TextInput
                id={id("value")}
                value={props.entry.value.clone()}
                placeholder="true"
                disabled={props.disabled}
                invalid={rejected}
                onchange={on_value}
            />

            <Button
                small=true
                kind={ButtonKind::Danger}
                disabled={props.disabled}
                title={Some(AttrValue::from("Remove this preference"))}
                onclick={on_remove}
            >
                { "Remove" }
            </Button>

            if rejected {
                <p class="pref-row__note pref-row__note--error" role="alert">
                    { format!(
                        "A {} entry cannot hold '{}'. ATAK drops an entry whose value does not \
                         match its class.",
                        props.entry.class.as_str(),
                        props.entry.value,
                    ) }
                </p>
            } else if let Some(known) = known {
                <p class="pref-row__note">
                    { known.description.clone() }
                    if let Some(default) = &known.default {
                        <span class="pref-row__default">
                            { format!(" ATAK's own default is {default}.") }
                        </span>
                    }
                </p>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, class: PrefClass, value: &str) -> PrefEntry {
        PrefEntry::new(key, class, value)
    }

    #[test]
    fn a_list_the_server_would_accept_has_no_problem() {
        let entries = vec![
            entry("deviceProfileEnableOnConnect", PrefClass::String, "true"),
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

    #[test]
    fn every_class_is_offered_and_every_option_parses_back() {
        let options = class_options();

        assert_eq!(options.len(), PrefClass::ALL.len());
        for option in options {
            assert!(PrefClass::parse(option.value.as_str()).is_some());
        }
    }
}
