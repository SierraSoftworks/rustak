//! The two kinds of row a preference list is drawn as. [`model::Catalog::row`]
//! says which one an entry is.
//!
//! Both share one grid — the entry's inputs, then its Remove button — so a
//! list that mixes them reads as one list.

use std::rc::Rc;

use rustak_api::{ConfigIssue, PrefClass, PrefEntry};
use serde_json::Value;
use yew::prelude::*;

use super::model::edited;
use crate::components::{Button, ButtonKind, Field, SchemaNode, Select, SelectOption, TextInput};

/// Emitted by a row: the entry after a change, or `None` to remove it.
pub type RowChange = Callback<Option<PrefEntry>>;

fn remove(onchange: &RowChange, disabled: bool) -> Html {
    html! {
        <Button
            small=true
            kind={ButtonKind::Danger}
            {disabled}
            title={Some(AttrValue::from("Remove this preference"))}
            onclick={onchange.reform(|_: MouseEvent| None)}
        >
            { "Remove" }
        </Button>
    }
}

#[derive(Properties, PartialEq)]
pub struct KnownRowProps {
    pub index: usize,
    pub entry: PrefEntry,
    /// The catalogue's whole schema, which the property is read against.
    pub root: Rc<Value>,
    pub property: Value,
    /// The class ATAK reads the key as, which is what the control edits.
    pub reads: PrefClass,
    pub value: Option<Value>,
    pub note: Option<String>,
    pub disabled: bool,
    pub onchange: RowChange,
}

/// A catalogued preference, drawn with the control its schema calls for. The
/// class it is sent as is one the catalogue accepts for it, so it is shown
/// rather than asked for — and kept as it is stored whatever is edited.
#[function_component(KnownRow)]
pub fn known_row(props: &KnownRowProps) -> Html {
    let pointer = format!("/{}/value", props.index);
    let label = props
        .property
        .get("title")
        .and_then(Value::as_str)
        .map_or_else(|| props.entry.key.clone(), str::to_string);
    let issues: Vec<ConfigIssue> = props
        .note
        .iter()
        .map(|note| ConfigIssue::at(pointer.clone(), note.clone()))
        .collect();

    let onchange = {
        let (entry, reads, onchange) = (props.entry.clone(), props.reads, props.onchange.clone());
        Callback::from(move |value: Option<Value>| {
            onchange.emit(Some(edited(&entry, reads, value)));
        })
    };

    html! {
        <div class="pref-row">
            <div class="pref-row__body">
                <SchemaNode
                    root={props.root.clone()}
                    schema={props.property.clone()}
                    pointer={pointer.clone()}
                    {label}
                    required=true
                    value={props.value.clone()}
                    {onchange}
                    issues={Rc::new(issues)}
                    disabled={props.disabled}
                    scope="pref"
                />
                <p class="pref-row__key">
                    <code>{ props.entry.key.clone() }</code>
                    { format!(" · sent as {}", props.entry.class.as_str()) }
                </p>
            </div>
            { remove(&props.onchange, props.disabled) }
        </div>
    }
}

fn class_options() -> Vec<SelectOption> {
    PrefClass::ALL
        .iter()
        .map(|class| SelectOption::new(class.as_str(), class.as_str()))
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct OtherRowProps {
    pub index: usize,
    pub entry: PrefEntry,
    /// Why a catalogued key is not drawn with its control.
    pub note: Option<String>,
    /// What the conversion button says, and the entry it writes.
    pub adopt: Option<(String, PrefEntry)>,
    pub disabled: bool,
    pub onchange: RowChange,
}

/// A preference the catalogue does not describe — or not as it is stored —
/// with its key, class and value written by hand, and sent exactly as written.
///
/// The key is handed back when its input loses the focus rather than on every
/// keystroke: typing a catalogued key would otherwise turn this row into that
/// key's control halfway through a word, and take the focus with it.
#[function_component(OtherRow)]
pub fn other_row(props: &OtherRowProps) -> Html {
    let key = use_state(|| props.entry.key.clone());
    {
        // What is stored changed from outside — a row above removed, a revert —
        // which is the one time the text should follow it.
        let key = key.clone();
        use_effect_with(props.entry.key.clone(), move |stored| {
            key.set(stored.clone());
            || ()
        });
    }

    let id = |part: &str| AttrValue::from(format!("pref-{}-{part}", props.index));
    let edit = |change: fn(&mut PrefEntry, String)| {
        let (entry, key, onchange) = (props.entry.clone(), key.clone(), props.onchange.clone());
        Callback::from(move |text: String| {
            let mut next = PrefEntry {
                key: (*key).clone(),
                ..entry.clone()
            };
            change(&mut next, text);
            onchange.emit(Some(next));
        })
    };
    let on_class = edit(|entry, text| {
        if let Some(class) = PrefClass::parse(&text) {
            entry.class = class;
        }
    });
    let on_key_blur = {
        let (stored, commit) = (
            props.entry.key.clone(),
            edit(|entry, text| entry.key = text),
        );
        Callback::from(move |text: String| {
            if text != stored {
                commit.emit(text);
            }
        })
    };

    let rejected = !props.entry.class.accepts(&props.entry.value);
    let error = rejected.then(|| {
        AttrValue::from(format!(
            "A {} entry cannot hold '{}'. ATAK drops an entry whose value does not match its class.",
            props.entry.class.as_str(),
            props.entry.value,
        ))
    });

    // Converting is offered, never done: what is stored is what was saved.
    let adopt = props.adopt.clone().map(|(label, adopted)| {
        let onclick = props
            .onchange
            .reform(move |_: MouseEvent| Some(adopted.clone()));
        (label, onclick)
    });

    html! {
        <div class="pref-row">
            <div class="pref-row__body">
                <div class="pref-row__free">
                    <Field label="Key" id={id("key")}>
                        <TextInput
                            id={id("key")}
                            value={(*key).clone()}
                            placeholder="com.example.plugin.setting"
                            disabled={props.disabled}
                            invalid={key.trim().is_empty()}
                            onchange={
                                let key = key.clone();
                                Callback::from(move |text: String| key.set(text))
                            }
                            onblur={on_key_blur}
                        />
                    </Field>
                    <Field label="Class" id={id("class")}>
                        <Select
                            id={id("class")}
                            value={Some(AttrValue::from(props.entry.class.as_str()))}
                            options={class_options()}
                            disabled={props.disabled}
                            onchange={on_class.reform(Option::unwrap_or_default)}
                        />
                    </Field>
                    <Field label="Value" id={id("value")} {error}>
                        <TextInput
                            id={id("value")}
                            value={props.entry.value.clone()}
                            disabled={props.disabled}
                            invalid={rejected}
                            onchange={edit(|entry, text| entry.value = text)}
                        />
                    </Field>
                </div>

                if let Some(note) = &props.note {
                    <p class="pref-row__note pref-row__note--warning">
                        { format!("{note} ") }
                        if let Some((label, onclick)) = adopt {
                            <Button small=true kind={ButtonKind::Subtle} disabled={props.disabled} {onclick}>
                                { label }
                            </Button>
                        }
                    </p>
                } else {
                    <p class="pref-row__note">
                        { "Not in the catalogue: sent exactly as written here." }
                    </p>
                }
            </div>
            { remove(&props.onchange, props.disabled) }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_is_offered_and_every_option_parses_back() {
        let options = class_options();

        assert_eq!(options.len(), PrefClass::ALL.len());
        for option in options {
            assert!(PrefClass::parse(option.value.as_str()).is_some());
        }
    }
}
