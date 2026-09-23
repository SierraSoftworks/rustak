//! One thing on the map, as a row of a list: the object list, or the chooser.
//!
//! The row is reduced to what it shows before it is drawn, so that a redraw
//! with nothing new in it compares equal and costs nothing.

use rustak_api::MapFeature;
use yew::prelude::*;

use crate::util::{short_relative, sidc};

use super::render::team_color;

/// The most rows drawn. A busy ADS-B feed is thousands of tracks, and a list
/// that long is not a list anybody reads — they search it.
pub const ROSTER_ROWS: usize = 150;

/// One row, already reduced to what it shows so that a redraw with nothing new
/// in it compares equal and costs nothing.
#[derive(Clone, Debug, PartialEq)]
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

/// One row of a list of things on the map, wherever the list is.
pub fn roster_row(entry: &RosterEntry, selected: bool, onselect: &Callback<String>) -> Html {
    let onclick = {
        let (onselect, uid) = (onselect.clone(), entry.uid.clone());
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
}
