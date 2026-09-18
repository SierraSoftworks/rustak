//! One mission, in four views.
//!
//! Overview, Subscribers, Changes and Layers are four different questions an
//! operator arrives with — what is this, who is on it, what happened, and how
//! is it arranged — and they are four shapes of data. Tabs rather than one
//! long page, because the reason somebody opened this is usually exactly one
//! of the four.
//!
//! Each tab lives in its own file: `mission_overview` carries the actions,
//! `mission_changes` its own request, and `mission_tabs` the two lists that
//! come out of the detail document itself.

use rustak_api::{MissionDetail, MissionGuid};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::app::Route;
use crate::components::{Alert, AlertKind, LoadingNote};
use crate::util::nav_href;

use super::load::{use_refresh_action, use_resource};
use super::mission_changes::MissionChanges;
use super::mission_overview::Overview;
use super::mission_tabs::{MissionLayers, MissionSubscribers};

/// The views this page offers, in the order an operator works through them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Subscribers,
    Changes,
    Layers,
}

impl Tab {
    const ALL: &'static [Self] = &[
        Self::Overview,
        Self::Subscribers,
        Self::Changes,
        Self::Layers,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Subscribers => "Subscribers",
            Self::Changes => "Changes",
            Self::Layers => "Layers",
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct MissionDetailPageProps {
    /// The mission's guid, straight out of the route.
    pub guid: String,
}

#[function_component(MissionDetailPage)]
pub fn mission_detail_page(props: &MissionDetailPageProps) -> Html {
    // A guid that could never have been stored is not a mission that is
    // missing — it is one that could not exist, and saying so beats a lookup
    // that fails for a reason nobody can act on.
    let Ok(guid) = MissionGuid::parse(&props.guid) else {
        return html! {
            <Alert
                kind={AlertKind::Error}
                title="That is not a mission identifier."
                message={format!(
                    "'{}' is not a guid this server could have issued. Missions are addressed \
                     by guid here, never by name.",
                    props.guid,
                )}
            />
        };
    };

    html! { <Detail {guid} /> }
}

#[derive(Properties, PartialEq)]
struct DetailProps {
    guid: MissionGuid,
}

#[function_component(Detail)]
fn detail(props: &DetailProps) -> Html {
    let guid = props.guid;
    let mission = use_resource(move || async move { api::missions::get(&guid).await });
    use_refresh_action(mission.reload.clone(), mission.busy);

    let tab = use_state(|| Tab::Overview);

    match (&mission.data, &mission.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not open that mission."
                message={message.clone()}
            />
        },
        (Some(found), _) => html! {
            <>
                <Heading mission={found.clone()} />
                <Tabs current={*tab} onselect={
                    let tab = tab.clone();
                    Callback::from(move |chosen: Tab| tab.set(chosen))
                } />
                { view(*tab, found, &mission.reload) }
            </>
        },
    }
}

fn view(tab: Tab, mission: &MissionDetail, reload: &Callback<()>) -> Html {
    match tab {
        Tab::Overview => html! { <Overview mission={mission.clone()} /> },
        Tab::Subscribers => html! {
            <MissionSubscribers mission={mission.clone()} on_changed={reload.clone()} />
        },
        Tab::Changes => html! { <MissionChanges guid={mission.summary.guid} /> },
        Tab::Layers => html! { <MissionLayers layers={mission.layers.clone()} /> },
    }
}

#[derive(Properties, PartialEq)]
struct HeadingProps {
    mission: MissionDetail,
}

#[function_component(Heading)]
fn heading(props: &HeadingProps) -> Html {
    let summary = &props.mission.summary;

    html! {
        <div class="entity-heading">
            <div>
                <h2 class="entity-heading__name">{ summary.name.clone() }</h2>
                <p class="entity-heading__meta">
                    <span class="entity-heading__username">{ summary.guid.to_string() }</span>
                    { " · " }
                    { summary.tool.clone() }
                </p>
            </div>
            { back_link() }
        </div>
    }
}

/// Demo mode lives in the query string, which a client-side navigation would
/// drop — so the way back out of a demo page is a plain link that carries it.
fn back_link() -> Html {
    if crate::fixtures::is_demo() {
        return html! {
            <a class="btn btn--small" href={nav_href("/admin/missions")}>{ "All missions" }</a>
        };
    }

    html! {
        <Link<Route> to={Route::Missions} classes="btn btn--small">{ "All missions" }</Link<Route>>
    }
}

#[derive(Properties, PartialEq)]
struct TabsProps {
    current: Tab,
    onselect: Callback<Tab>,
}

#[function_component(Tabs)]
fn tabs(props: &TabsProps) -> Html {
    html! {
        <div class="tabs" role="tablist">
            { for Tab::ALL.iter().map(|tab| {
                let tab = *tab;
                let active = tab == props.current;
                let onclick = {
                    let onselect = props.onselect.clone();
                    Callback::from(move |_: MouseEvent| onselect.emit(tab))
                };

                html! {
                    <button
                        type="button"
                        role="tab"
                        key={tab.label()}
                        class={classes!("tabs__tab", active.then_some("tabs__tab--active"))}
                        aria-selected={active.to_string()}
                        {onclick}
                    >
                        { tab.label() }
                    </button>
                }
            }) }
        </div>
    }
}
