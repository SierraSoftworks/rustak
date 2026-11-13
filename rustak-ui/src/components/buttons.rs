//! Buttons, as yewi draws them.

use yew::prelude::*;
use yewi::{Attrs, NavTarget, Size};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    #[default]
    Default,

    /// The one action a view exists for.
    Primary,

    /// An action that destroys or revokes something.
    Danger,

    /// An action that should not compete with the content around it.
    Subtle,
}

impl ButtonKind {
    pub fn variant(self) -> yewi::ButtonVariant {
        match self {
            Self::Default => yewi::ButtonVariant::Default,
            Self::Primary => yewi::ButtonVariant::Primary,
            Self::Danger => yewi::ButtonVariant::Danger,
            Self::Subtle => yewi::ButtonVariant::Subtle,
        }
    }
}

pub(super) fn size_of(small: bool, large: bool) -> Size {
    match (small, large) {
        (true, _) => Size::Small,
        (_, true) => Size::Large,
        _ => Size::Medium,
    }
}

#[derive(Properties, PartialEq)]
pub struct ButtonProps {
    #[prop_or_default]
    pub onclick: Callback<MouseEvent>,

    /// Where the button goes, for the ones that are links: an in-app path.
    /// Demo mode is carried across, as it is for every other link.
    #[prop_or_default]
    pub href: Option<AttrValue>,

    #[prop_or_default]
    pub kind: ButtonKind,

    #[prop_or_default]
    pub disabled: bool,

    /// Disabled, and says something is happening, so it cannot be sent twice.
    #[prop_or_default]
    pub busy: bool,

    #[prop_or_default]
    pub small: bool,

    #[prop_or_default]
    pub large: bool,

    #[prop_or_default]
    pub icon: Option<yewi::IconData>,

    #[prop_or_default]
    pub title: Option<AttrValue>,

    /// The accessible name, where it says more than the button's text.
    #[prop_or_default]
    pub aria_label: Option<AttrValue>,

    #[prop_or_default]
    pub children: Html,
}

#[function_component(Button)]
pub fn button(props: &ButtonProps) -> Html {
    let extra = [("title", &props.title), ("aria-label", &props.aria_label)]
        .into_iter()
        .filter_map(|(name, value)| value.clone().map(|value| (name, value)));
    let href = props
        .href
        .as_ref()
        .map(|path| NavTarget::href(crate::util::nav_href(path)))
        .unwrap_or_default();

    html! {
        <yewi::Button
            variant={props.kind.variant()}
            size={size_of(props.small, props.large)}
            icon={props.icon}
            disabled={props.disabled}
            loading={props.busy}
            {href}
            on_click={props.onclick.clone()}
            attrs={Attrs::new(extra)}
        >
            { props.children.clone() }
        </yewi::Button>
    }
}

#[derive(Properties, PartialEq)]
pub struct ButtonGroupProps {
    #[prop_or_default]
    pub label: Option<AttrValue>,

    #[prop_or_default]
    pub children: Html,
}

/// Several actions on one thing, joined into a single control.
#[function_component(ButtonGroup)]
pub fn button_group(props: &ButtonGroupProps) -> Html {
    html! {
        <yewi::ButtonGroup label={props.label.clone()}>
            { props.children.clone() }
        </yewi::ButtonGroup>
    }
}
