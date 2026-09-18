//! Data Sync missions, as an operator sees them.
//!
//! Not the list a TAK client is shown: this one hides nothing, so an
//! invite-only or password-protected mission is here too — somebody asking
//! what is on their server is asking about all of it.
//!
//! # Except for the deleted ones
//!
//! A deleted mission keeps its row, so that a client syncing late is told it
//! went rather than simply failing to find it — but `GET /api/v1/missions`
//! takes no `include_deleted`, so this listing cannot show one. The row below
//! renders `deleted_at` because the DTO carries it and the server fills it in
//! elsewhere; it is dead until that parameter exists. See the M3-04 status
//! file.

use rustak_api::{MissionGuid, MissionSummary};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::app::Route;
use crate::components::{
    Alert, AlertKind, Card, Field, LoadingNote, RoleBadge, StatusPill, StatusTone, TextInput,
};
use crate::util::{nav_href, short_relative};

use super::load::{use_refresh_action, use_resource};

#[function_component(Missions)]
pub fn missions() -> Html {
    let missions = use_resource(api::missions::list);
    use_refresh_action(missions.reload.clone(), missions.busy);

    let filter = use_state(String::new);

    let body = match (&missions.data, &missions.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the missions."
                message={message.clone()}
            />
        },
        (Some(list), _) => {
            let matching: Vec<&MissionSummary> = list
                .iter()
                .filter(|mission| matches_filter(mission, &filter))
                .collect();

            if matching.is_empty() {
                html! {
                    <p class="panel-empty">
                        { if list.is_empty() {
                            "Nothing here yet. A client creates a Data Sync mission from its \
                             own map, and it appears here."
                        } else {
                            "No mission matches that."
                        } }
                    </p>
                }
            } else {
                html! {
                    <ul class="mission-list">
                        { for matching.into_iter().map(|mission| html! {
                            <li key={mission.guid.to_string()}>
                                <MissionRow mission={mission.clone()} />
                            </li>
                        }) }
                    </ul>
                }
            }
        }
    };

    html! {
        <Card
            title="All missions"
            subtitle="Open one to see its subscribers, its changes and its layers."
        >
            <Field
                label="Filter"
                id="mission-filter"
                help="Matches name, tool, keyword or channel."
            >
                <TextInput
                    id="mission-filter"
                    value={(*filter).clone()}
                    placeholder="Kettle"
                    onchange={
                        let filter = filter.clone();
                        Callback::from(move |value: String| filter.set(value))
                    }
                />
            </Field>

            { body }
        </Card>
    }
}

/// Whether a mission matches what somebody typed, ignoring case.
fn matches_filter(mission: &MissionSummary, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }

    let haystack = [
        mission.name.clone(),
        mission.tool.clone(),
        mission.description.clone().unwrap_or_default(),
        mission.keywords.join(" "),
        mission
            .groups
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    ];

    haystack
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
}

/// The link from a listed mission to its own page.
fn detail_link(mission: &MissionSummary) -> Html {
    let label = mission.name.clone();
    let guid: MissionGuid = mission.guid;

    // Demo mode lives in the query string and a client-side navigation
    // replaces the whole URL, so a `Link` out of a demo page would land on one
    // talking to a server that is not there.
    if crate::fixtures::is_demo() {
        let href = nav_href(&format!("/admin/missions/{guid}"));
        return html! { <a class="mission-row__name mission-row__name--link" {href}>{ label }</a> };
    }

    html! {
        <Link<Route>
            to={Route::MissionDetail { guid: guid.to_string() }}
            classes="mission-row__name mission-row__name--link"
        >
            { label }
        </Link<Route>>
    }
}

#[derive(Properties, PartialEq)]
struct MissionRowProps {
    mission: MissionSummary,
}

#[function_component(MissionRow)]
fn mission_row(props: &MissionRowProps) -> Html {
    let mission = &props.mission;

    html! {
        <div class={classes!(
            "mission-row",
            mission.deleted_at.is_some().then_some("mission-row--deleted"),
        )}>
            <div class="mission-row__identity">
                { detail_link(mission) }
                if let Some(description) = &mission.description {
                    <span class="mission-row__description">{ description.clone() }</span>
                }
            </div>

            <div class="mission-row__meta">
                <span title="The tool a client filters this mission by">
                    { mission.tool.clone() }
                </span>
                <span title="The channels whose members may see it">
                    { match mission.groups.is_empty() {
                        true => "No channel".to_string(),
                        false => mission.groups
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    } }
                </span>
                <span>{ format!("{} subscribers", mission.subscriber_count) }</span>
                <span>{ format!("{} items", mission.uid_count) }</span>
                <span>{ format!("{} files", mission.content_count) }</span>
                <span title="When it was created">
                    { format!("Created {}", short_relative(mission.create_time)) }
                </span>
                if let Some(expiration) = mission.expiration {
                    <span title="When the server will reap it">
                        { format!("Expires {}", short_relative(expiration)) }
                    </span>
                }
                if !mission.keywords.is_empty() {
                    <span>{ mission.keywords.join(" · ") }</span>
                }
            </div>

            <div class="mission-row__state">
                if let Some(deleted) = mission.deleted_at {
                    <StatusPill
                        tone={StatusTone::Error}
                        label="Deleted"
                        title={format!(
                            "Deleted {}. Kept so a client syncing late is told it went.",
                            short_relative(deleted),
                        )}
                    />
                }
                if mission.password_protected {
                    <StatusPill
                        tone={StatusTone::Warning}
                        label="Password"
                        title="A subscriber must present the mission password."
                    />
                }
                if mission.invite_only {
                    <StatusPill
                        tone={StatusTone::Neutral}
                        label="Invite only"
                        title="Hidden from a client's own listing until it is invited."
                    />
                }
                <RoleBadge role={mission.default_role} />
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::{GroupName, MissionRoleKind};

    use super::*;

    fn mission() -> MissionSummary {
        MissionSummary {
            guid: MissionGuid::parse("0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f").unwrap(),
            name: "Operation Kettle".to_string(),
            description: None,
            tool: "public".to_string(),
            creator_uid: None,
            create_time: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            groups: vec![GroupName::from_storage("Command")],
            keywords: vec!["recon".to_string()],
            subscriber_count: 1,
            uid_count: 0,
            content_count: 0,
            password_protected: false,
            invite_only: false,
            default_role: MissionRoleKind::Subscriber,
            expiration: None,
            deleted_at: None,
        }
    }

    #[test]
    fn an_empty_filter_matches_everything() {
        assert!(matches_filter(&mission(), ""));
        assert!(matches_filter(&mission(), "   "));
    }

    #[test]
    fn the_filter_reaches_the_keyword_and_the_channel_as_well_as_the_name() {
        assert!(matches_filter(&mission(), "kettle"));
        assert!(
            matches_filter(&mission(), "RECON"),
            "keywords, ignoring case"
        );
        assert!(matches_filter(&mission(), "command"), "and channels");
        assert!(!matches_filter(&mission(), "rescue"));
    }
}
