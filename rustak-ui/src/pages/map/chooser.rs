//! The pop-over's other face: which of these did you mean?
//!
//! Shown when a click landed on more than one thing. The rows are the roster's
//! rows — the same swatch, name and detail — because it is the same question
//! the roster answers, asked of a handful of things instead of all of them.

use yew::prelude::*;

use super::roster::RosterEntry;

#[derive(Properties, PartialEq)]
pub struct ChooserProps {
    /// What was under the click, topmost first.
    pub entries: Vec<RosterEntry>,
    pub onselect: Callback<String>,
}

#[function_component(Chooser)]
pub fn chooser(props: &ChooserProps) -> Html {
    let row = |entry: &RosterEntry| {
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
                        entry.stale.then_some("map-roster__row--stale"),
                    )}
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

    html! {
        <section class="map-popover__card map-chooser" aria-label="Choose what to look at">
            <h3 class="map-chooser__title">
                { format!("{} things here", props.entries.len()) }
            </h3>
            <ul class="map-chooser__list">{ for props.entries.iter().map(row) }</ul>
        </section>
    }
}
