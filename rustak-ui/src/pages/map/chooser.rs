//! The pop-over's other face: which of these did you mean?
//!
//! Shown when a click landed on more than one thing. The rows are the roster's
//! rows — the same swatch, name and detail — because it is the same question
//! the roster answers, asked of a handful of things instead of all of them.

use yew::prelude::*;

use super::roster::{RosterEntry, roster_row};

#[derive(Properties, PartialEq)]
pub struct ChooserProps {
    /// What was under the click, topmost first.
    pub entries: Vec<RosterEntry>,
    pub onselect: Callback<String>,
}

#[function_component(Chooser)]
pub fn chooser(props: &ChooserProps) -> Html {
    // Nothing in the chooser is selected: whatever is chosen becomes the
    // focus, and the chooser is gone.
    let row = |entry: &RosterEntry| roster_row(entry, false, &props.onselect);

    html! {
        <section class="map-popover__card map-chooser" aria-label="Choose what to look at">
            <h3 class="map-chooser__title">
                { format!("{} things here", props.entries.len()) }
            </h3>
            <ul class="map-chooser__list">{ for props.entries.iter().map(row) }</ul>
        </section>
    }
}
