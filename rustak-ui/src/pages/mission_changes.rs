//! A mission's Changes tab.
//!
//! Its own file because it is its own request: the change log is not part of
//! the detail document, since a mission that has been running for a month has
//! more changes than everything else about it put together.

use rustak_api::{MissionChangeSummary, MissionGuid};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, LoadingNote, StatusPill, StatusTone, Switch};
use crate::util::{format_iso8601, short_relative};

use super::load::use_resource;

#[derive(Properties, PartialEq)]
pub struct MissionChangesProps {
    pub guid: MissionGuid,
}

/// The change log, squashed or in full.
///
/// The toggle is the difference between two real questions: "what happened
/// here" and "what would a device be told if it asked now". Squashing keeps
/// one entry per piece of content and drops what has since been removed, which
/// is exactly what a client syncing from scratch receives.
#[function_component(MissionChanges)]
pub fn mission_changes(props: &MissionChangesProps) -> Html {
    let squashed = use_state(|| false);
    let guid = props.guid;
    let wanted = *squashed;

    let changes = use_resource(move || async move { api::missions::changes(&guid, wanted).await });

    // `use_resource` fetches on mount and on reload, so flipping the toggle has
    // to ask for the fetch rather than merely changing what the next one would
    // send.
    {
        let reload = changes.reload.clone();
        use_effect_with(wanted, move |_| {
            reload.emit(());
            || ()
        });
    }

    let body = match (&changes.data, &changes.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the changes."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">{ "Nothing has changed in this mission yet." }</p>
        },
        (Some(list), _) => html! {
            <ul class="change-list">
                { for list.iter().enumerate().map(|(index, change)| html! {
                    <li key={index}><ChangeRow change={change.clone()} /></li>
                }) }
            </ul>
        },
    };

    html! {
        <Card title="Changes" subtitle="What has been added to and taken out of this mission.">
            <Switch
                id="mission-changes-squashed"
                checked={*squashed}
                label="Squashed — one entry per item, as a client syncing now would receive"
                disabled={changes.busy}
                onchange={
                    let squashed = squashed.clone();
                    Callback::from(move |value: bool| squashed.set(value))
                }
            />

            { body }
        </Card>
    }
}

/// What a change kind is called, and how loudly it is shown.
fn change_tone(change: &MissionChangeSummary) -> (StatusTone, &'static str) {
    use rustak_api::MissionChangeKind::*;

    match change.kind {
        CreateMission => (StatusTone::Ok, "Created"),
        DeleteMission => (StatusTone::Error, "Deleted"),
        AddContent => (StatusTone::Ok, "Added"),
        RemoveContent => (StatusTone::Warning, "Removed"),
        CreateDataFeed => (StatusTone::Ok, "Feed added"),
        DeleteDataFeed => (StatusTone::Warning, "Feed removed"),
    }
}

#[derive(Properties, PartialEq)]
struct ChangeRowProps {
    change: MissionChangeSummary,
}

#[function_component(ChangeRow)]
fn change_row(props: &ChangeRowProps) -> Html {
    let (tone, label) = change_tone(&props.change);
    let change = &props.change;
    let details = change
        .details
        .as_ref()
        .filter(|details| !details.is_empty());

    html! {
        <div class="change-row">
            <StatusPill {tone} {label} title={change.kind.as_str()} />

            <div class="change-row__identity">
                <span class="change-row__name">
                    { details
                        .and_then(|details| details.callsign.clone())
                        .or_else(|| change.content_uid.clone())
                        .unwrap_or_else(|| "—".to_string()) }
                </span>
                if let Some(uid) = &change.content_uid {
                    <span class="change-row__uid">{ uid.clone() }</span>
                }
            </div>

            <div class="change-row__meta">
                <span title={format_iso8601(change.timestamp)}>
                    { short_relative(change.timestamp) }
                </span>
                if let Some(creator) = &change.creator_uid {
                    <span title="The client that made the change">{ creator.clone() }</span>
                }
                if let Some(kind) = details.and_then(|details| details.kind.clone()) {
                    <span title="The CoT type">{ kind }</span>
                }
                if let (Some(lat), Some(lon)) = (
                    details.and_then(|details| details.lat),
                    details.and_then(|details| details.lon),
                ) {
                    <span>{ format!("{lat:.5}, {lon:.5}") }</span>
                }
                if let Some(hash) = &change.content_hash {
                    <span title="The content hash of the attached file">
                        { hash.chars().take(12).collect::<String>() }
                    </span>
                }
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::MissionChangeKind;

    use super::*;

    fn change(kind: MissionChangeKind) -> MissionChangeSummary {
        MissionChangeSummary {
            kind,
            content_uid: None,
            content_hash: None,
            timestamp: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            server_time: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            creator_uid: None,
            details: None,
        }
    }

    #[test]
    fn taking_something_away_is_never_shown_in_the_same_tone_as_adding_it() {
        for kind in MissionChangeKind::ALL.iter().copied() {
            let (tone, label) = change_tone(&change(kind));

            assert!(!label.is_empty());

            let taking_away = matches!(
                kind,
                MissionChangeKind::RemoveContent
                    | MissionChangeKind::DeleteDataFeed
                    | MissionChangeKind::DeleteMission
            );

            assert_eq!(
                taking_away,
                tone != StatusTone::Ok,
                "{kind:?} should stand apart exactly when it takes something away",
            );
        }
    }
}
