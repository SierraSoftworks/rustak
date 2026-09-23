//! Everything on the map, as a list laid over its corner.
//!
//! A map answers "what is near here"; it is bad at "where is RAO" and at "how
//! many aircraft are there". The list is the other way in: grouped by what
//! things *are* — the head of their CoT type, `a-u-G | Unknown · Ground` —
//! with each group folding away, and searched by callsign, uid or type. Pick
//! a row and the map goes there. It is also the way in for anybody who cannot
//! use a WebGL canvas with a pointer, which is reason enough.

use std::collections::{BTreeMap, HashSet};

use rustak_api::MapFeature;
use yew::prelude::*;

use crate::components::TextInput;
use crate::util::sidc;

use super::roster::{ROSTER_ROWS, RosterEntry, roster_row};

/// One fold of the list: everything of one kind.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectGroup {
    /// The head of the type, `a-u-G`.
    pub key: String,
    /// What that means, `Unknown · Ground`.
    pub label: String,
    pub entries: Vec<RosterEntry>,
}

/// The heads of the types that are not atoms, and what they hold.
const FAMILIES: &[(&str, &str)] = &[
    ("b-m-p", "Markers"),
    ("b-m-r", "Routes"),
    ("u-d", "Drawings"),
    ("u-r", "Range and bearing"),
    ("b-a", "Alerts"),
    ("b-i", "Images"),
    ("b-r", "Reports"),
];

/// `features` folded into groups, by type, in order of type. The features
/// are taken in the order given, which is the roster's: by name.
pub fn group(features: &[&MapFeature]) -> Vec<ObjectGroup> {
    let mut groups: BTreeMap<String, Vec<RosterEntry>> = BTreeMap::new();

    for feature in features.iter().take(ROSTER_ROWS) {
        groups
            .entry(key(&feature.kind))
            .or_default()
            .push(RosterEntry::of(feature));
    }

    groups
        .into_iter()
        .map(|(key, entries)| ObjectGroup {
            label: label(&key),
            key,
            entries,
        })
        .collect()
}

/// The head of a type: an atom's affiliation and dimension, or the family
/// anything else belongs to.
fn key(kind: &str) -> String {
    if let Some((family, _)) = FAMILIES
        .iter()
        .find(|(family, _)| kind == *family || kind.starts_with(&format!("{family}-")))
    {
        return (*family).to_string();
    }

    kind.split('-').take(3).collect::<Vec<_>>().join("-")
}

fn label(key: &str) -> String {
    if let Some((_, label)) = FAMILIES.iter().find(|(family, _)| *family == key) {
        return (*label).to_string();
    }

    sidc::describe(key).unwrap_or_else(|| "Other".to_string())
}

#[derive(Properties, PartialEq)]
pub struct ObjectListProps {
    pub groups: Vec<ObjectGroup>,

    /// How many matched, which is more than are listed when the list was cut.
    pub matched: usize,

    pub search: AttrValue,
    pub onsearch: Callback<String>,

    pub selected: Option<String>,
    pub onselect: Callback<String>,
}

#[function_component(ObjectList)]
pub fn object_list(props: &ObjectListProps) -> Html {
    let folded = use_state(HashSet::<String>::new);
    let hidden = use_state(|| false);

    let toggle_panel = {
        let hidden = hidden.clone();
        Callback::from(move |_: MouseEvent| hidden.set(!*hidden))
    };

    let listed: usize = props.groups.iter().map(|group| group.entries.len()).sum();
    let empty = match (listed == 0, props.search.is_empty()) {
        (false, _) => "",
        (true, true) => "Nothing is reporting. Whatever connects will appear here, and on the map.",
        (true, false) => "Nothing on the map matches.",
    };
    let more = match props.matched.saturating_sub(listed) {
        0 => String::new(),
        more => format!("And {more} more. Search to narrow them down."),
    };

    let group = |group: &ObjectGroup| {
        let open = !folded.contains(&group.key);
        let toggle = {
            let (folded, key) = (folded.clone(), group.key.clone());
            Callback::from(move |_: MouseEvent| {
                let mut next = (*folded).clone();
                if !next.remove(&key) {
                    next.insert(key.clone());
                }
                folded.set(next);
            })
        };

        html! {
            <li key={group.key.clone()} class="map-objects__group">
                <button
                    type="button"
                    class="map-objects__head"
                    aria-expanded={open.to_string()}
                    onclick={toggle}
                >
                    <span class="map-objects__chevron" aria-hidden="true">
                        { if open { "▾" } else { "▸" } }
                    </span>
                    <code class="map-objects__key">{ group.key.clone() }</code>
                    <span class="map-objects__label">{ group.label.clone() }</span>
                    <span class="map-objects__count">{ group.entries.len() }</span>
                </button>
                if open {
                    <ul class="map-objects__rows">
                        { for group.entries.iter().map(|entry| {
                            let selected = props.selected.as_deref() == Some(entry.uid.as_str());
                            roster_row(entry, selected, &props.onselect)
                        }) }
                    </ul>
                }
            </li>
        }
    };

    // The children of the panel never change in number: see `pages::map`.
    html! {
        <aside class={classes!("map-objects", hidden.then_some("map-objects--hidden"))} aria-label="On the map">
            <header class="map-objects__bar">
                <h2 class="map-objects__title">{ "On the map" }</h2>
                <button
                    type="button"
                    class="map-objects__fold"
                    aria-label={if *hidden { "Show the list" } else { "Hide the list" }}
                    aria-expanded={(!*hidden).to_string()}
                    onclick={toggle_panel}
                >
                    { if *hidden { "▸" } else { "▴" } }
                </button>
            </header>
            <div class="map-objects__body">
                <TextInput
                    id="map-search"
                    value={props.search.clone()}
                    onchange={props.onsearch.clone()}
                    placeholder="Search callsigns, uids or types"
                />
                <ul class="map-objects__groups">{ for props.groups.iter().map(group) }</ul>
                <p class="map-objects__empty">{ empty }</p>
                <p class="map-objects__more">{ more }</p>
            </div>
        </aside>
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::MapPoint;

    use super::*;

    fn feature(uid: &str, kind: &str) -> MapFeature {
        MapFeature {
            uid: uid.to_string(),
            kind: kind.to_string(),
            how: None,
            callsign: Some(uid.to_string()),
            team: None,
            role: None,
            time: "2026-09-18T12:00:00Z".parse().unwrap(),
            stale: "2026-09-18T12:10:00Z".parse().unwrap(),
            received_at: "2026-09-18T12:00:00Z".parse().unwrap(),
            point: MapPoint {
                lat: 51.5,
                lon: -0.12,
                hae: None,
                ce: None,
                le: None,
            },
            shape: None,
            course: None,
            speed: None,
            battery: None,
            remarks: None,
            software: None,
            sidc: None,
            groups: Vec::new(),
        }
    }

    #[test]
    fn things_are_grouped_by_the_head_of_their_type_and_named_for_it() {
        let features = [
            feature("RAO", "a-f-G-U-C"),
            feature("HELO", "a-f-A-C-H"),
            feature("QUINN", "a-f-G-U-C-I"),
            feature("CCP", "b-m-p-s-m"),
            feature("ROUTE", "b-m-r"),
            feature("CORDON", "u-d-f"),
            feature("ODD", "x-y-z-w"),
        ];
        let grouped = group(&features.iter().collect::<Vec<_>>());

        let heads: Vec<(&str, &str, usize)> = grouped
            .iter()
            .map(|group| {
                (
                    group.key.as_str(),
                    group.label.as_str(),
                    group.entries.len(),
                )
            })
            .collect();

        assert_eq!(
            heads,
            [
                ("a-f-A", "Friendly · Air", 1),
                ("a-f-G", "Friendly · Ground", 2),
                ("b-m-p", "Markers", 1),
                ("b-m-r", "Routes", 1),
                ("u-d", "Drawings", 1),
                ("x-y-z", "Other", 1),
            ]
        );
        assert_eq!(grouped[1].entries[0].uid, "RAO", "in the order given");
    }
}
