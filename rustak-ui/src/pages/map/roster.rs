//! Everything on the map, as a list beside it.
//!
//! A map answers "what is near here"; it is bad at "where is RAO". The roster
//! is the other way in: search by callsign, uid or type, pick a row, and the
//! map goes there and opens its pop-over. It is also the way in for anybody
//! who cannot use a WebGL canvas with a pointer, which is reason enough.

use rustak_api::MapFeature;
use yew::prelude::*;

use crate::components::TextInput;
use crate::util::{short_relative, sidc};

use super::render::team_color;

/// The most rows drawn. A busy ADS-B feed is thousands of tracks, and a list
/// that long is not a list anybody reads — they search it.
pub const ROSTER_ROWS: usize = 150;

/// One row, already reduced to what it shows so that a redraw with nothing new
/// in it compares equal and costs nothing.
#[derive(Clone, PartialEq)]
pub struct RosterEntry {
    pub uid: String,
    pub name: String,
    pub detail: String,
    pub swatch: &'static str,
    pub stale: bool,
}

impl RosterEntry {
    pub fn of(feature: &MapFeature) -> Self {
        Self {
            uid: feature.uid.clone(),
            name: feature
                .callsign
                .clone()
                .unwrap_or_else(|| feature.uid.clone()),
            detail: format!(
                "{} · {}",
                sidc::describe(&feature.kind).unwrap_or_else(|| feature.kind.clone()),
                short_relative(feature.time),
            ),
            swatch: match &feature.team {
                Some(team) => team_color(team),
                None => affiliation_color(&feature.kind),
            },
            stale: feature.stale < chrono::Utc::now(),
        }
    }
}

/// The fill MIL-STD-2525 gives each affiliation's frame, so that a row's
/// swatch is the colour of the symbol it stands for.
fn affiliation_color(kind: &str) -> &'static str {
    match kind.split('-').nth(1).filter(|_| kind.starts_with("a-")) {
        Some("f" | "a") => "#80e0ff",
        Some("h" | "s" | "j" | "k") => "#ff8080",
        Some("n") => "#aaffaa",
        Some(_) => "#ffff80",
        None => "#98a2b3",
    }
}

#[derive(Properties, PartialEq)]
pub struct RosterProps {
    pub entries: Vec<RosterEntry>,

    /// How many matched, which is more than `entries` when the list was cut.
    pub matched: usize,

    pub search: AttrValue,
    pub onsearch: Callback<String>,

    pub selected: Option<String>,
    pub onselect: Callback<String>,
}

#[function_component(Roster)]
pub fn roster(props: &RosterProps) -> Html {
    let row = |entry: &RosterEntry| {
        let selected = props.selected.as_deref() == Some(entry.uid.as_str());
        let onclick = {
            let (onselect, uid) = (props.onselect.clone(), entry.uid.clone());
            Callback::from(move |_: MouseEvent| onselect.emit(uid.clone()))
        };

        html! {
            <li key={entry.uid.clone()}>
                <button
                    type="button"
                    class={classes!(
                        "map-roster__row",
                        selected.then_some("map-roster__row--selected"),
                        entry.stale.then_some("map-roster__row--stale"),
                    )}
                    aria-pressed={selected.to_string()}
                    {onclick}
                >
                    <span
                        class="map-roster__swatch"
                        style={format!("background: {}", entry.swatch)}
                        aria-hidden="true"
                    />
                    <span class="map-roster__name">{ entry.name.clone() }</span>
                    <span class="map-roster__detail">{ entry.detail.clone() }</span>
                </button>
            </li>
        }
    };

    let empty = match (props.entries.is_empty(), props.search.is_empty()) {
        (false, _) => "",
        (true, true) => "Nothing is reporting. Whatever connects will appear here, and on the map.",
        (true, false) => "Nothing on the map matches.",
    };
    let more = match props.matched.saturating_sub(props.entries.len()) {
        0 => String::new(),
        more => format!("And {more} more. Search to narrow them down."),
    };

    // Four children, always: a paragraph with nothing to say is empty and
    // hidden rather than absent. Yew reconciles un-keyed siblings by position,
    // so one that came and went would take the search box with it — and the
    // focus of whoever was typing in it.
    html! {
        <aside class="map-roster" aria-label="On the map">
            <TextInput
                id="map-search"
                value={props.search.clone()}
                onchange={props.onsearch.clone()}
                placeholder="Search callsigns, uids or types"
            />
            <ul class="map-roster__list">{ for props.entries.iter().map(row) }</ul>
            <p class="map-roster__empty">{ empty }</p>
            <p class="map-roster__more">{ more }</p>
        </aside>
    }
}
