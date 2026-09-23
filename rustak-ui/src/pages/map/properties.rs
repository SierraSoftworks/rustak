//! The properties panel, laid over the right of the map: what the thing in
//! focus is, and — when a person put it there — the fields to change it.
//!
//! Everything a TAK client's own details pane leads with — who, what, where,
//! how fast, how fresh — then what only the server knows: which channels the
//! sender was publishing into, which is the first thing to check when
//! somebody says they cannot see it. A marker somebody placed is a form on
//! top of that: the name, the type, the symbol, where it is, what it is for,
//! and where it goes.

use rustak_api::MapFeature;
use yew::prelude::*;

use crate::components::{Alert, AlertKind, Button, ButtonKind, ConfirmButton, TextArea, TextInput};
use crate::util::{short_relative, sidc};

use super::draft::{Draft, TYPES, editable};
use super::facts::facts;

#[derive(Properties, PartialEq)]
pub struct PropertiesProps {
    pub feature: MapFeature,
    /// Whether `feature` is the live one rather than a moment from its past.
    /// Only the live one is edited.
    pub live: bool,
    /// The channels the signed-in account may publish into.
    pub channels: Vec<String>,
    /// Why the last save or delete did not happen.
    #[prop_or_default]
    pub problem: Option<String>,
    #[prop_or_default]
    pub busy: bool,

    pub onclose: Callback<()>,
    pub onsave: Callback<Draft>,
    pub ondelete: Callback<String>,
}

#[function_component(Properties)]
pub fn properties(props: &PropertiesProps) -> Html {
    let feature = &props.feature;
    let name = feature
        .callsign
        .clone()
        .unwrap_or_else(|| feature.uid.clone());
    let stale = feature.stale < chrono::Utc::now();

    let onclose = {
        let onclose = props.onclose.clone();
        Callback::from(move |_: MouseEvent| onclose.emit(()))
    };

    // Folded away to its heading, the panel still says what is in focus —
    // and the focus, with its track and its playback, is untouched. On a
    // phone the panel is a third of the map, and watching something move is
    // the one time it is in the way.
    let folded = use_state(|| false);
    let onfold = {
        let folded = folded.clone();
        Callback::from(move |_: MouseEvent| folded.set(!*folded))
    };

    html! {
        <article class="map-properties" aria-label={format!("Details for {name}")}>
            <header class="map-properties__head">
                <div>
                    <h2 class="map-properties__name">{ name.clone() }</h2>
                    <p class="map-properties__kind">
                        if let Some(described) = sidc::describe(&feature.kind) {
                            { described }{ " · " }
                        }
                        <code>{ feature.kind.clone() }</code>
                    </p>
                </div>
                <div class="map-properties__buttons">
                    <button
                        type="button"
                        class="map-properties__close"
                        aria-label={if *folded { "Show the details" } else { "Hide the details" }}
                        aria-expanded={(!*folded).to_string()}
                        onclick={onfold}
                    >
                        { if *folded { "▸" } else { "▾" } }
                    </button>
                    <button
                        type="button"
                        class="map-properties__close"
                        aria-label="Close"
                        onclick={onclose}
                    >
                        { "×" }
                    </button>
                </div>
            </header>

            // Hidden rather than unmounted, so that what was being typed is
            // still there when it comes back.
            <div class={classes!("map-properties__body", folded.then_some("map-properties__body--folded"))}>
                if editable(feature) && !props.live {
                    <p class="map-properties__hint">{ "Return to live to edit this marker." }</p>
                }
                if editable(feature) && props.live {
                    <Editor
                        feature={feature.clone()}
                        channels={props.channels.clone()}
                        problem={props.problem.clone()}
                        busy={props.busy}
                        onsave={props.onsave.clone()}
                        ondelete={props.ondelete.clone()}
                    />
                }

                <dl class="map-properties__facts">
                    { for facts(feature).into_iter().map(|(term, value)| html! {
                        <>
                            <dt>{ term }</dt>
                            <dd>{ value }</dd>
                        </>
                    }) }
                </dl>

                if !(editable(feature) && props.live) {
                    if let Some(remarks) = &feature.remarks {
                        <p class="map-properties__remarks">{ remarks.clone() }</p>
                    }
                }

                <p class={classes!("map-properties__age", stale.then_some("map-properties__age--stale"))}>
                    { format!("Reported {}", short_relative(feature.time)) }
                    { " · " }
                    { if stale {
                        format!("stale since {}", short_relative(feature.stale))
                    } else {
                        format!("stale {}", short_relative(feature.stale))
                    } }
                </p>
            </div>
        </article>
    }
}

#[derive(Properties, PartialEq)]
struct EditorProps {
    feature: MapFeature,
    channels: Vec<String>,
    problem: Option<String>,
    busy: bool,
    onsave: Callback<Draft>,
    ondelete: Callback<String>,
}

/// The fields of a marker a person placed.
///
/// The draft is this component's own until it is saved, and starts again from
/// the feature whenever a *different* feature arrives — so an edit in
/// progress survives the feed re-announcing the same marker, and does not
/// survive somebody selecting another one.
#[function_component(Editor)]
fn editor(props: &EditorProps) -> Html {
    let draft = use_state(|| Draft::of(&props.feature));
    {
        let draft = draft.clone();
        use_effect_with(props.feature.uid.clone(), {
            let feature = props.feature.clone();
            move |_| {
                draft.set(Draft::of(&feature));
                || ()
            }
        });
    }

    let field = |apply: fn(&mut Draft, String)| {
        let draft = draft.clone();
        Callback::from(move |value: String| {
            let mut next = (*draft).clone();
            apply(&mut next, value);
            draft.set(next);
        })
    };
    let toggle_channel = |channel: String| {
        let draft = draft.clone();
        Callback::from(move |_: Event| {
            let mut next = (*draft).clone();
            match next.groups.iter().position(|held| *held == channel) {
                Some(at) => {
                    next.groups.remove(at);
                }
                None => next.groups.push(channel.clone()),
            }
            draft.set(next);
        })
    };
    // Only the channels the form shows are sent: see `Draft::within`.
    let onsave = {
        let (onsave, draft, channels) =
            (props.onsave.clone(), draft.clone(), props.channels.clone());
        Callback::from(move |_: MouseEvent| onsave.emit((*draft).clone().within(&channels)))
    };
    let ondelete = {
        let (ondelete, uid) = (props.ondelete.clone(), props.feature.uid.clone());
        Callback::from(move |()| ondelete.emit(uid.clone()))
    };

    html! {
        <form class="map-editor" onsubmit={Callback::from(|e: SubmitEvent| e.prevent_default())}>
            <label class="map-editor__field">
                <span>{ "Name" }</span>
                <TextInput id="marker-name" value={draft.callsign.clone()} onchange={field(|d, v| d.callsign = v)} />
            </label>
            <label class="map-editor__field">
                <span>{ "Type" }</span>
                <TextInput
                    id="marker-type"
                    value={draft.kind.clone()}
                    onchange={field(|d, v| d.kind = v)}
                    list="marker-types"
                    placeholder="a-u-G"
                />
                <datalist id="marker-types">
                    { for TYPES.iter().map(|(kind, label)| html! {
                        <option value={*kind} label={*label} />
                    }) }
                </datalist>
            </label>
            <label class="map-editor__field">
                <span>{ "Symbol" }</span>
                <TextInput
                    id="marker-sidc"
                    value={draft.sidc.clone()}
                    onchange={field(|d, v| d.sidc = v)}
                    placeholder="MIL-STD-2525 code, or leave to the type"
                />
            </label>
            <div class="map-editor__row">
                <label class="map-editor__field">
                    <span>{ "Latitude" }</span>
                    <TextInput id="marker-lat" value={draft.lat.clone()} onchange={field(|d, v| d.lat = v)} />
                </label>
                <label class="map-editor__field">
                    <span>{ "Longitude" }</span>
                    <TextInput id="marker-lon" value={draft.lon.clone()} onchange={field(|d, v| d.lon = v)} />
                </label>
                <label class="map-editor__field">
                    <span>{ "Altitude (m HAE)" }</span>
                    <TextInput id="marker-hae" value={draft.hae.clone()} onchange={field(|d, v| d.hae = v)} placeholder="Unknown" />
                </label>
            </div>
            <label class="map-editor__field">
                <span>{ "Remarks" }</span>
                <TextArea id="marker-remarks" value={draft.remarks.clone()} onchange={field(|d, v| d.remarks = v)} rows={3} />
            </label>
            <fieldset class="map-editor__channels">
                <legend>{ "Publish into" }</legend>
                if props.channels.is_empty() {
                    <p class="map-editor__hint">{ "Everybody you can reach." }</p>
                }
                { for props.channels.iter().map(|channel| html! {
                    <label key={channel.clone()} class="map-editor__channel">
                        <input
                            type="checkbox"
                            checked={draft.groups.contains(channel)}
                            onchange={toggle_channel(channel.clone())}
                        />
                        { channel.clone() }
                    </label>
                }) }
            </fieldset>

            if let Some(problem) = &props.problem {
                <Alert kind={AlertKind::Error} title="Not saved" message={problem.clone()} />
            }

            <div class="map-editor__actions">
                <Button kind={ButtonKind::Primary} small=true busy={props.busy} onclick={onsave}>{ "Save" }</Button>
                <ConfirmButton
                    label="Delete"
                    question="Delete this marker from every map and device?"
                    confirm_label="Delete it"
                    disabled={props.busy}
                    onconfirm={ondelete}
                />
            </div>
        </form>
    }
}
