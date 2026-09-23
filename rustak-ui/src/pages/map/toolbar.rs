//! The tools, laid over the top of the map.
//!
//! One of them is always chosen. *Select* is what a click has always done;
//! *pin* puts a marker where the next click lands; and the drawing tools —
//! one for each [`Form`] ATAK's own drawing tools make — take clicks until
//! there is a drawing, with the [`SketchBar`] saying what the next click does.
//! Every one of them hands back to select when it is done. Beside them, what
//! the page used to say above the map: whether the feed is live, how much is
//! on it, and a way to fit it all into view.

use yew::prelude::*;

use crate::components::{StatusPill, StatusTone};

use super::geometry::Form;

/// What a click on the map does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Opens whatever is under it.
    #[default]
    Select,
    /// Places a marker there.
    Pin,
    /// Adds to a drawing there.
    Draw(Form),
}

impl Tool {
    fn label(self) -> String {
        match self {
            Self::Select => "Select".to_string(),
            Self::Pin => "Place a marker".to_string(),
            Self::Draw(form) => format!("Draw a {}", form.label().to_lowercase()),
        }
    }

    /// The inside of a 20 by 20 icon.
    fn icon(self) -> Html {
        let outline = |d: &'static str| {
            html! {
                <path {d} fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" />
            }
        };

        match self {
            Self::Select => {
                html! { <path d="M4 2l12 9h-6l3 6-2 1-3-6-4 4z" fill="currentColor" /> }
            }
            Self::Pin => html! {
                <path d="M10 1a6 6 0 0 0-6 6c0 4.5 6 12 6 12s6-7.5 6-12a6 6 0 0 0-6-6zm0 8.5A2.5 2.5 0 1 1 10 4.5a2.5 2.5 0 0 1 0 5z" fill="currentColor" />
            },
            Self::Draw(Form::Line) => outline("M3 15l5-9 4 6 5-8"),
            Self::Draw(Form::Polygon) => outline("M10 3l7 5-3 9H6L3 8z"),
            Self::Draw(Form::Rectangle) => outline("M3 5h14v10H3z"),
            Self::Draw(Form::Circle) => outline("M10 3a7 7 0 1 0 0 14a7 7 0 1 0 0-14z"),
            Self::Draw(Form::Route) => html! {
                <>
                    { outline("M5 16c0-7 10-2 10-10") }
                    <circle cx="5" cy="16" r="2.2" fill="currentColor" />
                    <circle cx="15" cy="5" r="2.2" fill="currentColor" />
                </>
            },
        }
    }
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
    let tool = |which: Tool| {
        let ontool = props.ontool.clone();
        let pressed = props.tool == which;

        html! {
            <button
                type="button"
                class={classes!("map-toolbar__tool", pressed.then_some("map-toolbar__tool--active"))}
                aria-label={which.label()}
                aria-pressed={pressed.to_string()}
                title={which.label()}
                onclick={Callback::from(move |_: MouseEvent| ontool.emit(which))}
            >
                <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">{ which.icon() }</svg>
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
                { tool(Tool::Select) }
                { tool(Tool::Pin) }
            </div>
            <div class="map-toolbar__group" role="group" aria-label="Draw">
                { for Form::ALL.into_iter().map(|form| tool(Tool::Draw(form))) }
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

#[derive(Properties, PartialEq)]
pub struct SketchBarProps {
    /// What the next click does.
    pub hint: AttrValue,
    pub can_undo: bool,
    pub can_finish: bool,
    pub onundo: Callback<()>,
    pub onfinish: Callback<()>,
    pub oncancel: Callback<()>,
}

/// What is said, and what can be done, while something is being drawn. The
/// buttons are what a finger has instead of a double click and a keyboard.
#[function_component(SketchBar)]
pub fn sketch_bar(props: &SketchBarProps) -> Html {
    let button = |label: &'static str, enabled: bool, action: &Callback<()>| {
        let action = action.clone();

        html! {
            <button
                type="button"
                class="map-sketch__action"
                disabled={!enabled}
                onclick={Callback::from(move |_: MouseEvent| action.emit(()))}
            >
                { label }
            </button>
        }
    };

    html! {
        <div class="map-sketch">
            <p class="map-sketch__hint" role="status">{ props.hint.clone() }</p>
            <div class="map-sketch__actions">
                { button("Undo", props.can_undo, &props.onundo) }
                { button("Finish", props.can_finish, &props.onfinish) }
                { button("Cancel", true, &props.oncancel) }
            </div>
        </div>
    }
}
