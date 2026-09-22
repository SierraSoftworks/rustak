//! The tools, laid over the top of the map.
//!
//! Two tools, one of them always chosen: *select*, which is what a click has
//! always done, and *pin*, which puts a marker where the next click lands and
//! then hands back to select. Beside them, what the page used to say above
//! the map: whether the feed is live, how much is on it, and a way to fit it
//! all into view.

use yew::prelude::*;

use crate::components::{StatusPill, StatusTone};

/// What a click on the map does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Opens whatever is under it.
    #[default]
    Select,
    /// Places a marker there.
    Pin,
}

#[derive(Properties, PartialEq)]
pub struct ToolbarProps {
    pub tool: Tool,
    pub ontool: Callback<Tool>,
    pub onfit: Callback<()>,

    pub tone: StatusTone,
    pub label: AttrValue,
    #[prop_or_default]
    pub title: Option<AttrValue>,
    /// How much is on the map.
    pub count: usize,
}

#[function_component(Toolbar)]
pub fn toolbar(props: &ToolbarProps) -> Html {
    let tool = |which: Tool, label: &'static str, icon: Html| {
        let ontool = props.ontool.clone();
        let pressed = props.tool == which;

        html! {
            <button
                type="button"
                class={classes!("map-toolbar__tool", pressed.then_some("map-toolbar__tool--active"))}
                aria-label={label}
                aria-pressed={pressed.to_string()}
                title={label}
                onclick={Callback::from(move |_: MouseEvent| ontool.emit(which))}
            >
                { icon }
            </button>
        }
    };
    let onfit = {
        let onfit = props.onfit.clone();
        Callback::from(move |_: MouseEvent| onfit.emit(()))
    };

    html! {
        <div class="map-toolbar" role="toolbar" aria-label="Map tools">
            <div class="map-toolbar__group">
                { tool(Tool::Select, "Select", html! {
                    <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
                        <path d="M4 2l12 9h-6l3 6-2 1-3-6-4 4z" fill="currentColor" />
                    </svg>
                }) }
                { tool(Tool::Pin, "Place a marker", html! {
                    <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
                        <path d="M10 1a6 6 0 0 0-6 6c0 4.5 6 12 6 12s6-7.5 6-12a6 6 0 0 0-6-6zm0 8.5A2.5 2.5 0 1 1 10 4.5a2.5 2.5 0 0 1 0 5z" fill="currentColor" />
                    </svg>
                }) }
            </div>
            <div class="map-toolbar__group">
                <button
                    type="button"
                    class="map-toolbar__tool"
                    aria-label="Fit everything"
                    title="Fit everything"
                    onclick={onfit}
                >
                    <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
                        <path d="M2 7V2h5v2H4v3zm11-5h5v5h-2V4h-3zM2 13h2v3h3v2H2zm14 0h2v5h-5v-2h3z" fill="currentColor" />
                    </svg>
                </button>
            </div>
            <div class="map-toolbar__group map-toolbar__status">
                <StatusPill tone={props.tone} label={props.label.clone()} title={props.title.clone()} />
                <span class="map-toolbar__count">
                    { match props.count {
                        1 => "1 thing".to_string(),
                        count => format!("{count} things"),
                    } }
                </span>
            </div>
        </div>
    }
}
