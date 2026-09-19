//! What is connected to the stream listener right now.
//!
//! The only page in the console that refreshes itself, and it has to: the
//! answer changes on its own, and an operator watching an exercise start is
//! looking for a device to *appear*. Five seconds, stopped while a request is
//! still in flight, and switchable off — a page left open on a wall display
//! should not be a request every five seconds for a week.
//!
//! # Disconnecting is not revoking
//!
//! Closing a socket does not stop the device reconnecting a moment later with
//! the same certificate. Taking access away is revoking that certificate,
//! which is on the Devices page — and the confirmation here says so rather
//! than implying an outcome this button does not have.
//!
//! # An empty list is two different answers
//!
//! `GET /clients` says `[]` for a quiet exercise and for an installation with
//! no stream listener. `GET /clients/status` is what tells them apart, and this
//! page asks it once so that the empty state can say which one it is looking
//! at instead of describing both.

use rustak_api::{ClientHistoryEntry, ConnectedClient, StreamStatus};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Card, ConfirmButton, LoadingNote, StatusPill, StatusTone, Switch,
};
use crate::util::{format_iso8601, short_relative};

use super::load::{use_refresh_action, use_resource};

/// How often a live list re-reads itself.
const REFRESH_MS: u32 = 5_000;

/// How far back the history looks when nobody says: a day, which is the window
/// an operator asking "was it ever here?" means.
const HISTORY_SECONDS: i64 = 86_400;

#[function_component(Clients)]
pub fn clients() -> Html {
    let clients = use_resource(api::clients::list);
    let history = use_resource(|| api::clients::history(HISTORY_SECONDS));
    let listener = use_resource(api::clients::status);

    let live = use_state(|| true);

    let reload = {
        let reloads = [clients.reload.clone(), history.reload.clone()];
        Callback::from(move |_| {
            for reload in &reloads {
                reload.emit(());
            }
        })
    };
    use_refresh_action(reload.clone(), clients.busy || history.busy);

    // Not an interval: a timer that fires while a request is still in flight
    // stacks them up on a slow link, and a page left open would then be
    // holding more connections the slower the server got. One timeout,
    // re-armed after each render that is not busy, cannot.
    {
        let (live, busy) = (*live, clients.busy);
        let reload = clients.reload.clone();
        use_effect_with((live, busy), move |(live, busy)| {
            let handle = (*live && !*busy)
                .then(|| gloo_timers::callback::Timeout::new(REFRESH_MS, move || reload.emit(())));

            move || drop(handle)
        });
    }

    let connected = clients.data.clone().unwrap_or_default();

    html! {
        <>
            <Card
                title="Connected now"
                subtitle="Every device holding a stream connection to this server."
            >
                <Switch
                    id="clients-live"
                    checked={*live}
                    label={format!("Refresh every {} seconds", REFRESH_MS / 1000)}
                    onchange={
                        let live = live.clone();
                        Callback::from(move |on: bool| live.set(on))
                    }
                />

                // How long the listener has been up, whether or not anybody
                // is connected to it: a client list that is shorter than it
                // should be reads very differently against a listener that
                // bound three days ago and one that bound a minute ago.
                if let Some(bound_at) = listener.data.as_ref().and_then(|it| it.bound_at) {
                    <p class="panel-note" title={format_iso8601(bound_at)}>
                        { format!("The stream listener bound {}.", short_relative(bound_at)) }
                    </p>
                }

                { connected_list(&clients.data, &clients.error, &listener.data, &reload) }
            </Card>

            <Card
                title="Seen in the last day"
                subtitle="Including the devices that are not connected at the moment."
            >
                { history_list(&history.data, &history.error, &connected) }
            </Card>
        </>
    }
}

fn connected_list(
    data: &Option<Vec<ConnectedClient>>,
    error: &Option<String>,
    listener: &Option<StreamStatus>,
    reload: &Callback<()>,
) -> Html {
    match (data, error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the connected clients."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">{ empty_reason(listener) }</p>
        },
        (Some(list), _) => html! {
            <ul class="client-list">
                { for list.iter().map(|client| html! {
                    <li key={client.client_uid.clone()}>
                        <ClientRow client={client.clone()} on_changed={reload.clone()} />
                    </li>
                }) }
            </ul>
        },
    }
}

fn history_list(
    data: &Option<Vec<ClientHistoryEntry>>,
    error: &Option<String>,
    connected: &[ConnectedClient],
) -> Html {
    match (data, error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the client history."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">{ "Nothing has connected in the last day." }</p>
        },
        (Some(list), _) => html! {
            <ul class="client-list">
                { for list.iter().map(|entry| {
                    let live = connected
                        .iter()
                        .any(|client| client.client_uid == entry.client_uid);

                    html! {
                        <li key={entry.client_uid.clone()}>
                            <HistoryRow entry={entry.clone()} {live} />
                        </li>
                    }
                }) }
            </ul>
        },
    }
}

#[derive(Properties, PartialEq)]
struct ClientRowProps {
    client: ConnectedClient,
    on_changed: Callback<()>,
}

#[function_component(ClientRow)]
fn client_row(props: &ClientRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let client = &props.client;

    let act = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let uid = client.client_uid.clone();

        Callback::from(move |incognito: Option<bool>| {
            let uid = uid.clone();
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());

            busy.set(true);
            spawn_local(async move {
                // The incognito call answers the updated connection; the list
                // is re-read either way, because disconnecting changes its
                // length and the page shows both.
                let outcome = match incognito {
                    Some(on) => api::clients::set_incognito(&uid, on).await.map(|_| ()),
                    None => api::clients::disconnect(&uid).await,
                };

                match outcome {
                    Ok(()) => {
                        error.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    html! {
        <div class="client-row">
            <div class="client-row__identity">
                <span class="client-row__name">{ client.callsign.clone() }</span>
                <span class="client-row__uid">{ client.client_uid.clone() }</span>
            </div>

            <div class="client-row__meta">
                <span title="The account it enrolled as">{ client.username.clone() }</span>
                <span>{ format!("{} · {}", client.team, client.role) }</span>
                <span>{ client.takv.clone() }</span>
                <span title="The protocol it negotiated">{ client.protocol.clone() }</span>
                <span title="Where it is connecting from">
                    { format!("{}:{}", client.ip, client.port) }
                </span>
                <span title={format_iso8601(client.connected_at)}>
                    { format!("Connected {}", short_relative(client.connected_at)) }
                </span>
                <span title={format_iso8601(client.last_event_at)}>
                    { format!("Last event {}", short_relative(client.last_event_at)) }
                </span>
                <span title="The channels it publishes into and receives from">
                    { channels(client) }
                </span>
            </div>

            <Switch
                id={format!("client-incognito-{}", client.client_uid)}
                checked={client.incognito}
                label="Incognito"
                disabled={*busy}
                onchange={
                    let act = act.clone();
                    Callback::from(move |on: bool| act.emit(Some(on)))
                }
            />

            <ConfirmButton
                label="Disconnect"
                confirm_label="Disconnect it"
                question={format!(
                    "Disconnect '{}'? It may reconnect immediately with the same certificate — \
                     revoke that on the Devices page to stop it.",
                    client.callsign,
                )}
                busy={*busy}
                onconfirm={
                    let act = act.clone();
                    Callback::from(move |_| act.emit(None))
                }
            />

            if let Some(message) = &*error {
                <p class="client-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

/// Why the list is empty, once the listener has said whether it is running.
///
/// Until it has, the page says the ambiguous thing — which is honest for the
/// moment before the answer arrives, and is replaced rather than corrected.
fn empty_reason(listener: &Option<StreamStatus>) -> &'static str {
    match listener {
        Some(status) if !status.enabled => {
            "Nothing is connected. This server has no stream listener — switch on \
             [stream.tls] in its configuration to let devices connect."
        }
        Some(status) if !status.bound => {
            "Nothing is connected. The stream listener is switched on but has not \
             come up; its start-up failure is in the server log."
        }
        Some(_) => "Nothing is connected. The stream listener is running and quiet.",
        None => "Nothing is connected.",
    }
}

/// What this client publishes into and receives from, in one phrase.
fn channels(client: &ConnectedClient) -> String {
    let names = |held: &[rustak_api::GroupName]| match held.is_empty() {
        true => "none".to_string(),
        false => held
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    };

    format!(
        "out: {} · in: {}",
        names(&client.out_groups),
        names(&client.in_groups),
    )
}

#[derive(Properties, PartialEq)]
struct HistoryRowProps {
    entry: ClientHistoryEntry,
    live: bool,
}

#[function_component(HistoryRow)]
fn history_row(props: &HistoryRowProps) -> Html {
    let entry = &props.entry;

    html! {
        <div class="client-row">
            <div class="client-row__identity">
                <span class="client-row__name">
                    { entry.callsign.clone().unwrap_or_else(|| "No callsign".to_string()) }
                </span>
                <span class="client-row__uid">{ entry.client_uid.clone() }</span>
            </div>

            <div class="client-row__meta">
                <span>{ entry.username.clone() }</span>
                if let Some(team) = &entry.team {
                    <span>
                        { match &entry.role {
                            Some(role) => format!("{team} · {role}"),
                            None => team.clone(),
                        } }
                    </span>
                }
                if let Some(takv) = &entry.takv {
                    <span>{ takv.clone() }</span>
                }
                if let Some(ip) = entry.last_ip {
                    <span title="Where we last saw it connect from">{ ip.to_string() }</span>
                }
                <span title={format_iso8601(entry.first_seen_at)}>
                    { format!("First seen {}", short_relative(entry.first_seen_at)) }
                </span>
                <span title={format_iso8601(entry.last_seen_at)}>
                    { format!("Last seen {}", short_relative(entry.last_seen_at)) }
                </span>
            </div>

            <StatusPill
                tone={if props.live { StatusTone::Ok } else { StatusTone::Neutral }}
                label={if props.live { "Connected" } else { "Offline" }}
            />
        </div>
    }
}
