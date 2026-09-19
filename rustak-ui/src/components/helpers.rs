//! The small pieces of furniture every page needs: a centred panel, a card, a
//! loading line, an empty state, a figure, and the refresh button pages push
//! into the shared title row.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CenterProps {
    pub children: Html,
}

/// Centres its children in the viewport. Used for sign-in and status screens.
#[function_component(Center)]
pub fn center(props: &CenterProps) -> Html {
    html! { <div class="center-screen">{ props.children.clone() }</div> }
}

#[derive(Properties, PartialEq)]
pub struct CardProps {
    /// The heading. A card with no title is a plain panel, which is sometimes
    /// what a page wants.
    #[prop_or_default]
    pub title: Option<AttrValue>,

    /// A supporting line under the heading.
    #[prop_or_default]
    pub subtitle: Option<AttrValue>,

    /// Controls aligned to the end of the heading row: the action the card
    /// exists for — a "Save", a "Mint" — or a link out of it. Up here rather
    /// than among the fields it acts on, so it is never lost among them.
    #[prop_or_default]
    pub actions: Html,

    #[prop_or_default]
    pub children: Html,
}

/// A bordered panel grouping one subject's worth of content.
#[function_component(Card)]
pub fn card(props: &CardProps) -> Html {
    let header = match (&props.title, &props.subtitle) {
        (None, None) => html! {},
        (title, subtitle) => html! {
            <div class="card__header">
                <div>
                    if let Some(title) = title {
                        <h2 class="card__title">{ title.clone() }</h2>
                    }
                    if let Some(subtitle) = subtitle {
                        <p class="card__subtitle">{ subtitle.clone() }</p>
                    }
                </div>
                <div class="card__actions">{ props.actions.clone() }</div>
            </div>
        },
    };

    html! {
        <section class="card">
            { header }
            <div class="card__body">{ props.children.clone() }</div>
        </section>
    }
}

#[derive(Properties, PartialEq)]
pub struct LoadingNoteProps {
    #[prop_or(AttrValue::from("Loading…"))]
    pub label: AttrValue,
}

/// What a page says while it is waiting.
#[function_component(LoadingNote)]
pub fn loading_note(props: &LoadingNoteProps) -> Html {
    html! { <p class="loading-note">{ props.label.clone() }</p> }
}

#[derive(Properties, PartialEq)]
pub struct EmptyStateProps {
    pub title: AttrValue,

    /// What to do about it, when there is something to do. An empty state that
    /// only says "nothing here" leaves the reader to guess whether that is a
    /// problem.
    #[prop_or_default]
    pub message: Option<AttrValue>,

    #[prop_or_default]
    pub children: Html,
}

#[function_component(EmptyState)]
pub fn empty_state(props: &EmptyStateProps) -> Html {
    html! {
        <div class="empty-state">
            <p class="empty-state__title">{ props.title.clone() }</p>
            if let Some(message) = &props.message {
                <p class="empty-state__message">{ message.clone() }</p>
            }
            { props.children.clone() }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct StatProps {
    pub label: AttrValue,
    pub value: AttrValue,

    /// A qualifier under the figure — a unit, a comparison, or the reason it is
    /// the number it is.
    #[prop_or_default]
    pub detail: Option<AttrValue>,

    #[prop_or_default]
    pub children: Html,
}

/// One figure on the dashboard.
#[function_component(Stat)]
pub fn stat(props: &StatProps) -> Html {
    html! {
        <div class="stat">
            <span class="stat__label">{ props.label.clone() }</span>
            <span class="stat__value">{ props.value.clone() }</span>
            if let Some(detail) = &props.detail {
                <span class="stat__detail">{ detail.clone() }</span>
            }
            { props.children.clone() }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct RefreshButtonProps {
    pub onclick: Callback<MouseEvent>,

    /// Spins the glyph and disables the button while a reload is in flight, so
    /// a slow request cannot be asked for twice.
    #[prop_or_default]
    pub busy: bool,
}

#[function_component(RefreshButton)]
pub fn refresh_button(props: &RefreshButtonProps) -> Html {
    html! {
        <button
            type="button"
            class="btn btn--small"
            onclick={props.onclick.clone()}
            disabled={props.busy}
            title="Reload this page's data"
            aria-label="Reload this page's data"
        >
            <span class={classes!(
                "refresh-btn__icon",
                props.busy.then_some("refresh-btn__icon--spin"),
            )}>
                <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
                    stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
                    aria-hidden="true">
                    <polyline points="23 4 23 10 17 10" />
                    <polyline points="1 20 1 14 7 14" />
                    <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
                </svg>
            </span>
            { "Refresh" }
        </button>
    }
}
