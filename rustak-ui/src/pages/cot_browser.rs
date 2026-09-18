//! The latest situational-awareness message this server holds for each uid.
//!
//! This is the page somebody opens when a marker is not where it should be,
//! and the question is always the same: what did the server actually receive,
//! and who was it entitled to reach? So the row carries the type, the
//! coordinates and the channels, and the drawer carries the bytes.
//!
//! Selecting a row opens [`CotDrawer`], which carries the bytes and the
//! history — see `cot_drawer` for why the document matters more than any
//! summary of it.

use rustak_api::{CotSummary, Group, GroupName};
use yew::prelude::*;

use crate::api;
use crate::api::cot::CotFilter;
use crate::components::{
    Alert, AlertKind, Card, Field, LoadingNote, Select, SelectOption, StatusPill, StatusTone,
    TextInput,
};
use crate::util::{format_iso8601, short_relative};

use super::cot_drawer::CotDrawer;
use super::load::{use_refresh_action, use_resource};

#[function_component(CotBrowser)]
pub fn cot_browser() -> Html {
    let filter = use_state(CotFilter::default);
    let wanted = (*filter).clone();

    let messages = use_resource(move || {
        let wanted = wanted.clone();
        async move { api::cot::list(&wanted).await }
    });
    use_refresh_action(messages.reload.clone(), messages.busy);

    let channels = use_resource(api::groups::list);
    let selected = use_state(|| None::<String>);

    // `use_resource` fetches on mount and on reload, so a change to the filter
    // has to ask for a fetch rather than merely changing what the next one
    // would send.
    {
        let reload = messages.reload.clone();
        use_effect_with((*filter).clone(), move |_| {
            reload.emit(());
            || ()
        });
    }

    let body = match (&messages.data, &messages.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the stored messages."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "Nothing matches. This server holds the latest message per identifier, so an \
                   installation nothing has connected to is empty." }
            </p>
        },
        (Some(list), _) => html! {
            <ul class="cot-list">
                { for list.iter().map(|summary| html! {
                    <li key={summary.uid.clone()}>
                        <CotRow
                            summary={summary.clone()}
                            selected={selected.as_deref() == Some(summary.uid.as_str())}
                            onselect={
                                let (selected, uid) = (selected.clone(), summary.uid.clone());
                                Callback::from(move |_: MouseEvent| {
                                    selected.set((selected.as_deref() != Some(uid.as_str()))
                                        .then(|| uid.clone()));
                                })
                            }
                        />
                    </li>
                }) }
            </ul>
        },
    };

    html! {
        <>
            <Card
                title="Latest message per identifier"
                subtitle="What this server last received from each device, and who it reached."
            >
                { filters(&filter, &channels.data) }
                { body }
            </Card>

            if let Some(uid) = selected.as_deref() {
                <CotDrawer
                    uid={uid.to_string()}
                    on_forgotten={
                        let (selected, reload) = (selected.clone(), messages.reload.clone());
                        Callback::from(move |_| {
                            selected.set(None);
                            reload.emit(());
                        })
                    }
                />
            }
        </>
    }
}

/// The three ways of narrowing the list, which are the three the endpoint
/// takes.
fn filters(filter: &UseStateHandle<CotFilter>, channels: &Option<Vec<Group>>) -> Html {
    let options: Vec<SelectOption> = channels
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|channel| SelectOption::new(channel.name.to_string(), channel.name.to_string()))
        .collect();

    html! {
        <div class="inline-form">
            <Field
                label="Type"
                id="cot-type"
                help="A prefix: 'a-' for atoms, 'a-f' for friendly, 'b-t-f' for chat."
            >
                <TextInput
                    id="cot-type"
                    value={filter.kind.clone()}
                    placeholder="a-f"
                    onchange={
                        let filter = filter.clone();
                        Callback::from(move |value: String| {
                            filter.set(CotFilter { kind: value, ..(*filter).clone() });
                        })
                    }
                />
            </Field>

            <Field label="Callsign" id="cot-callsign" help="Part of one, ignoring case.">
                <TextInput
                    id="cot-callsign"
                    value={filter.callsign.clone()}
                    placeholder="QUINN"
                    onchange={
                        let filter = filter.clone();
                        Callback::from(move |value: String| {
                            filter.set(CotFilter { callsign: value, ..(*filter).clone() });
                        })
                    }
                />
            </Field>

            <Field
                label="Channel"
                id="cot-group"
                help="Only the messages whose sender was publishing into it."
            >
                <Select
                    id="cot-group"
                    value={filter.group.as_ref().map(|group| AttrValue::from(group.to_string()))}
                    options={options}
                    clearable=true
                    placeholder="Any channel"
                    onchange={
                        let filter = filter.clone();
                        Callback::from(move |chosen: Option<String>| {
                            filter.set(CotFilter {
                                group: chosen.map(|name| GroupName::from_storage(&name)),
                                ..(*filter).clone()
                            });
                        })
                    }
                />
            </Field>
        </div>
    }
}

/// Whether a message has gone stale, which is what decides if a client still
/// draws it.
fn is_stale(summary: &CotSummary) -> bool {
    summary.stale <= chrono::Utc::now()
}

#[derive(Properties, PartialEq)]
struct CotRowProps {
    summary: CotSummary,
    selected: bool,
    onselect: Callback<MouseEvent>,
}

#[function_component(CotRow)]
fn cot_row(props: &CotRowProps) -> Html {
    let summary = &props.summary;
    let stale = is_stale(summary);

    html! {
        <div class={classes!("cot-row", props.selected.then_some("cot-row--selected"))}>
            <button
                type="button"
                class="cot-row__select"
                aria-pressed={props.selected.to_string()}
                onclick={props.onselect.clone()}
            >
                <span class="cot-row__name">
                    { summary.callsign.clone().unwrap_or_else(|| summary.uid.clone()) }
                </span>
                <span class="cot-row__uid">{ summary.uid.clone() }</span>
            </button>

            <div class="cot-row__meta">
                <span title="The CoT type">{ summary.kind.clone() }</span>
                if let Some(team) = &summary.team {
                    <span>
                        { match &summary.role {
                            Some(role) => format!("{team} · {role}"),
                            None => team.clone(),
                        } }
                    </span>
                }
                <span title="Where it said it was">
                    { format!("{:.5}, {:.5}", summary.lat, summary.lon) }
                </span>
                <span title={format_iso8601(summary.time)}>
                    { format!("Sent {}", short_relative(summary.time)) }
                </span>
                <span title={format_iso8601(summary.received_at)}>
                    { format!("Received {}", short_relative(summary.received_at)) }
                </span>
                <span title="The channels its sender was publishing into">
                    { match summary.groups.is_empty() {
                        true => "No channel".to_string(),
                        false => summary.groups.join(", "),
                    } }
                </span>
            </div>

            <StatusPill
                tone={if stale { StatusTone::Neutral } else { StatusTone::Ok }}
                label={if stale { "Stale" } else { "Current" }}
                title={format!(
                    "Stale at {}. A client stops drawing it after that.",
                    format_iso8601(summary.stale),
                )}
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(stale_in_minutes: i64) -> CotSummary {
        CotSummary {
            uid: "ANDROID-1".to_string(),
            kind: "a-f-G-U-C".to_string(),
            callsign: Some("ALPHA".to_string()),
            team: None,
            role: None,
            time: chrono::Utc::now(),
            stale: chrono::Utc::now() + chrono::Duration::minutes(stale_in_minutes),
            received_at: chrono::Utc::now(),
            lat: 51.5,
            lon: -0.1,
            groups: Vec::new(),
        }
    }

    #[test]
    fn a_message_whose_stale_time_has_passed_is_stale() {
        assert!(is_stale(&summary(-1)));
        assert!(!is_stale(&summary(2)));
    }
}
