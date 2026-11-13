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

use crate::api;
use crate::components::{
    Alert, AlertKind, LoadingNote, PageHeading, StatusPill, StatusTone, TabBar,
};
use crate::util::{format_iso8601, short_relative};

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
                { heading(found) }
                { tabs(
                    *tab,
                    {
                        let tab = tab.clone();
                        Callback::from(move |chosen: Tab| tab.set(chosen))
                    },
                    view(*tab, found, &mission.reload),
                ) }
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

/// Which mission this is, in the shell's title row, with the way back to the
/// list. A deleted mission says so here, so it is said on every tab.
fn heading(mission: &MissionDetail) -> Html {
    let summary = &mission.summary;
    let subtitle = html! {
        <>
            <code>{ summary.guid.to_string() }</code>
            { " · " }
            { summary.tool.clone() }
        </>
    };
    let meta = match summary.deleted_at {
        Some(at) => html! {
            <StatusPill
                tone={StatusTone::Neutral}
                label={format!("Deleted {}", short_relative(at))}
                title={Some(AttrValue::from(format_iso8601(at)))}
            />
        },
        None => Html::default(),
    };

    html! {
        <PageHeading
            title={summary.name.clone()}
            {subtitle}
            {meta}
            parent="Missions"
            parent_path="/admin/missions"
        />
    }
}

/// The tab strip, with the chosen tab's view under it.
fn tabs(current: Tab, onselect: Callback<Tab>, view: Html) -> Html {
    let onselect = onselect.reform(|label: &'static str| {
        Tab::ALL
            .iter()
            .copied()
            .find(|tab| tab.label() == label)
            .unwrap_or(Tab::ALL[0])
    });

    html! {
        <TabBar
            tabs={Tab::ALL.iter().map(|tab| tab.label()).collect::<Vec<_>>()}
            current={current.label()}
            {onselect}
        >
            { view }
        </TabBar>
    }
}
