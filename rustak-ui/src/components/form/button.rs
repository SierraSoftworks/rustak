//! Buttons.

use yew::prelude::*;

/// How prominent a [`Button`] should be.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    /// The ordinary action on a page.
    #[default]
    Default,

    /// The one action a page exists for. At most one per view, or none is.
    Primary,

    /// An action that destroys something.
    Danger,

    /// An action that should be available without drawing the eye.
    Subtle,
}

impl ButtonKind {
    fn class(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Primary => Some("btn--primary"),
            Self::Danger => Some("btn--danger"),
            Self::Subtle => Some("btn--subtle"),
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct ButtonProps {
    pub onclick: Callback<MouseEvent>,

    #[prop_or_default]
    pub kind: ButtonKind,

    #[prop_or_default]
    pub disabled: bool,

    /// Disables the button and shows that something is happening, so a slow
    /// request cannot be submitted twice.
    #[prop_or_default]
    pub busy: bool,

    #[prop_or_default]
    pub small: bool,

    #[prop_or_default]
    pub large: bool,

    /// Used when the label alone does not describe the action.
    #[prop_or_default]
    pub title: Option<AttrValue>,

    /// The name for anything reading the page aloud, for a button whose label
    /// is only a glyph.
    #[prop_or_default]
    pub aria_label: Option<AttrValue>,

    /// The label. `Html` rather than `Children` so that a caller can hand the
    /// text in as a value — `{ if disabled { "Restore" } else { "Suspend" } }` —
    /// which is what a button whose label depends on state has to do.
    #[prop_or_default]
    pub children: Html,
}

#[function_component(Button)]
pub fn button(props: &ButtonProps) -> Html {
    html! {
        <button
            type="button"
            class={classes!(
                "btn",
                props.kind.class(),
                props.small.then_some("btn--small"),
                props.large.then_some("btn--lg"),
                props.busy.then_some("btn--busy"),
            )}
            onclick={props.onclick.clone()}
            disabled={props.disabled || props.busy}
            title={props.title.clone()}
            aria-label={props.aria_label.clone()}
            aria-busy={props.busy.then_some("true")}
        >
            { props.children.clone() }
        </button>
    }
}

#[derive(Properties, PartialEq)]
pub struct ButtonGroupProps {
    /// What the set of actions is for, for anything reading the page aloud. A
    /// group with no name is one a screen reader can only announce as a group.
    #[prop_or_default]
    pub label: Option<AttrValue>,

    #[prop_or_default]
    pub children: Html,
}

/// Several actions on one thing, joined into a single control.
///
/// The shared edge is what says they belong together, and it costs less width
/// than the gaps between free-standing buttons — which is what a row has to give
/// up to say what the actions are being done to.
#[function_component(ButtonGroup)]
pub fn button_group(props: &ButtonGroupProps) -> Html {
    html! {
        <div class="btn-group" role="group" aria-label={props.label.clone()}>
            { props.children.clone() }
        </div>
    }
}
