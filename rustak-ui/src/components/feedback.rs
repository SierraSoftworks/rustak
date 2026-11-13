//! What a page says about its own state: a message, a status, that it is
//! loading, that there is nothing here — and the section each of those sits in.

use rustak_api::{AuditOutcome, ComponentStatus};
use yew::prelude::*;
use yewi::{Attrs, Tone, attrs};

/// How much an [`Alert`] asks of its reader.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // The full set is the component's API, not its current use.
pub enum AlertKind {
    Error,
    Warning,
    Info,
    Success,
}

impl AlertKind {
    fn tone(self) -> Tone {
        match self {
            AlertKind::Error => Tone::Danger,
            AlertKind::Warning => Tone::Warn,
            AlertKind::Info => Tone::Info,
            AlertKind::Success => Tone::Ok,
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct AlertProps {
    pub kind: AlertKind,

    pub title: AttrValue,

    #[prop_or_default]
    pub message: Option<AttrValue>,

    /// Draws a dismiss button, and is fired when it is pressed.
    #[prop_or_default]
    pub on_close: Option<Callback<()>>,

    /// What can be done about it: a button or two under the message.
    #[prop_or_default]
    pub children: Html,
}

/// A message about something that happened, where it happened.
#[function_component(Alert)]
pub fn alert(props: &AlertProps) -> Html {
    html! {
        <yewi::Alert
            tone={props.kind.tone()}
            title={props.title.clone()}
            actions={props.children.clone()}
            on_dismiss={props.on_close.clone()}
            dismiss_label="Dismiss"
        >
            { props.message.clone() }
        </yewi::Alert>
    }
}

/// What a [`StatusPill`] says about the thing beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Ok,
    Warning,
    Error,
    Neutral,
}

impl StatusTone {
    pub fn of_outcome(outcome: AuditOutcome) -> Self {
        match outcome {
            AuditOutcome::Success => StatusTone::Ok,
            AuditOutcome::Failure => StatusTone::Error,
            AuditOutcome::Denied => StatusTone::Warning,
            AuditOutcome::Skipped => StatusTone::Neutral,
        }
    }

    pub fn of_component(status: ComponentStatus) -> Self {
        match status {
            ComponentStatus::Ok => StatusTone::Ok,
            ComponentStatus::Degraded => StatusTone::Warning,
            ComponentStatus::Down => StatusTone::Error,
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            StatusTone::Ok => Tone::Ok,
            StatusTone::Warning => Tone::Warn,
            StatusTone::Error => Tone::Danger,
            StatusTone::Neutral => Tone::Neutral,
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct StatusPillProps {
    pub tone: StatusTone,

    pub label: AttrValue,

    #[prop_or_default]
    pub title: Option<AttrValue>,
}

/// A coloured state label, so a condition looks the same wherever it appears.
#[function_component(StatusPill)]
pub fn status_pill(props: &StatusPillProps) -> Html {
    let attrs = props
        .title
        .clone()
        .map(|title| attrs! { "title" => title })
        .unwrap_or_default();

    html! {
        <yewi::Pill tone={props.tone.tone()} class="status-pill" {attrs}>
            { props.label.clone() }
        </yewi::Pill>
    }
}

#[derive(Properties, PartialEq)]
pub struct TagProps {
    #[prop_or_default]
    pub title: Option<AttrValue>,

    #[prop_or_default]
    pub mono: bool,

    pub children: Html,
}

/// A short, unactionable label: a capability, a kind, a role.
#[function_component(Tag)]
pub fn tag(props: &TagProps) -> Html {
    let attrs = props
        .title
        .clone()
        .map(|title| attrs! { "title" => title })
        .unwrap_or_default();

    html! {
        <yewi::Tag class="tag" mono={props.mono} {attrs}>{ props.children.clone() }</yewi::Tag>
    }
}

#[derive(Properties, PartialEq)]
pub struct CenterProps {
    pub children: Html,
}

/// Centres its content in the viewport: sign-in, the wizard, a refusal.
#[function_component(Center)]
pub fn center(props: &CenterProps) -> Html {
    html! { <div class="center-screen">{ props.children.clone() }</div> }
}

#[derive(Properties, PartialEq)]
pub struct SectionProps {
    #[prop_or_default]
    pub title: Option<AttrValue>,

    #[prop_or_default]
    pub subtitle: Option<AttrValue>,

    /// Controls at the end of the heading row.
    #[prop_or_default]
    pub actions: Html,

    #[prop_or_default]
    pub children: Html,
}

/// A section of a page: its heading row, then its content flat on the page.
///
/// The content of a page is not boxed. A [`Callout`] is for the one thing on
/// it that should stand apart.
#[function_component(Section)]
pub fn section(props: &SectionProps) -> Html {
    html! {
        <yewi::Panel
            fill={yewi::PanelFill::Plain}
            level={2}
            class="section"
            title={props.title.clone()}
            subtitle={props.subtitle.clone()}
            actions={props.actions.clone()}
        >
            { props.children.clone() }
        </yewi::Panel>
    }
}

#[derive(Properties, PartialEq)]
pub struct CalloutProps {
    #[prop_or_default]
    pub title: Option<AttrValue>,

    #[prop_or_default]
    pub subtitle: Option<AttrValue>,

    #[prop_or_default]
    pub actions: Html,

    #[prop_or_default]
    pub children: Html,
}

/// A card, for a distinct piece of context within a page's flow: a summary
/// beside others like it, a form that stands apart from the list under it.
#[function_component(Callout)]
pub fn callout(props: &CalloutProps) -> Html {
    html! {
        <yewi::Card
            level={2}
            class="callout"
            title={props.title.clone()}
            subtitle={props.subtitle.clone()}
            actions={props.actions.clone()}
        >
            { props.children.clone() }
        </yewi::Card>
    }
}

#[derive(Properties, PartialEq)]
pub struct LoadingNoteProps {
    #[prop_or(AttrValue::from("Loading…"))]
    pub label: AttrValue,
}

#[function_component(LoadingNote)]
pub fn loading_note(props: &LoadingNoteProps) -> Html {
    html! { <yewi::Loading layout={yewi::LoadingLayout::Block} label={props.label.clone()} /> }
}

#[derive(Properties, PartialEq)]
pub struct EmptyStateProps {
    pub title: AttrValue,

    #[prop_or_default]
    pub message: Option<AttrValue>,

    /// What to do about it, when nothing being here is a problem.
    #[prop_or_default]
    pub children: Html,
}

#[function_component(EmptyState)]
pub fn empty_state(props: &EmptyStateProps) -> Html {
    html! {
        <yewi::EmptyState title={props.title.clone()} actions={props.children.clone()}>
            { props.message.clone() }
        </yewi::EmptyState>
    }
}

#[derive(Properties, PartialEq)]
pub struct StatProps {
    pub label: AttrValue,
    pub value: AttrValue,

    #[prop_or_default]
    pub detail: Option<AttrValue>,

    /// Where the number leads, for a figure that is also a way in.
    #[prop_or_default]
    pub href: Option<AttrValue>,
}

/// One figure and what it counts.
#[function_component(Stat)]
pub fn stat(props: &StatProps) -> Html {
    let href = props
        .href
        .as_ref()
        .map(|path| yewi::NavTarget::href(crate::util::nav_href(path)))
        .unwrap_or_default();

    html! {
        <yewi::StatTile
            label={props.label.clone()}
            value={props.value.clone()}
            detail={props.detail.clone()}
            variant={yewi::StatTileVariant::Plain}
            {href}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct RefreshButtonProps {
    pub onclick: Callback<MouseEvent>,

    #[prop_or_default]
    pub busy: bool,
}

/// Fetches the page's data again; spins while any fetch is in flight.
#[function_component(RefreshButton)]
pub fn refresh_button(props: &RefreshButtonProps) -> Html {
    html! {
        <yewi::Button
            icon={yewi::icons::REFRESH_CW}
            loading={props.busy}
            on_click={props.onclick.clone()}
            attrs={attrs! { "title" => "Reload this page's data" }}
        >
            { "Refresh" }
        </yewi::Button>
    }
}

/// The attributes for a `title` that may not be there.
pub fn title_attrs(title: &Option<AttrValue>) -> Attrs {
    title
        .clone()
        .map(|title| attrs! { "title" => title })
        .unwrap_or_default()
}
