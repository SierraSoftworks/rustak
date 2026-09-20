//! The end-user devices: everything that has enrolled, and which of them is
//! connected right now.
//!
//! Two lists joined on the device uid. `GET /devices` is what enrolled and
//! what it presented; `GET /clients` is what holds a stream connection this
//! second, with the address, the protocol and the channels each is on. A row
//! is a device, decorated with its connection when it has one — and a client
//! that is connected without ever having enrolled here gets a row of its own,
//! because a device the console cannot account for is the one worth seeing.
//!
//! # Connected by default
//!
//! An operator watching an exercise is looking for a device to *appear*, so
//! the list opens narrowed to what is connected and re-reads itself every few
//! seconds. Widening it shows every device that has enrolled, and brings up
//! the day's disconnections beneath — the answer to "was it ever here?".
//!
//! The refresh is a timeout re-armed after each render that is not busy, not
//! an interval: a timer that fires while a request is still in flight stacks
//! them up on a slow link.

use rustak_api::{ClientHistoryEntry, ConnectedClient, Device};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, Field, LoadingNote, Switch, TextInput};
use crate::util::{format_iso8601, short_relative};

use super::eud_rows::{ConnectedOnlyRow, HistoryRow, empty_reason};
use super::load::{use_refresh_action, use_resource};
use super::panels::{DeviceRow, certificate_for, matches_filter};

/// How often the live list re-reads itself.
const REFRESH_MS: u32 = 5_000;

/// How far back the history looks: a day, which is the window an operator
/// asking "was it ever here?" means.
const HISTORY_SECONDS: i64 = 86_400;

#[function_component(Euds)]
pub fn euds() -> Html {
    let devices = use_resource(|| api::devices::list(None));
    let certificates = use_resource(|| api::certificates::list(None, None));
    let clients = use_resource(api::clients::list);
    let history = use_resource(|| api::clients::history(HISTORY_SECONDS));
    let listener = use_resource(api::clients::status);

    let connected_only = use_state(|| true);
    let live = use_state(|| true);
    let filter = use_state(String::new);

    let reload = {
        let reloads = [
            devices.reload.clone(),
            certificates.reload.clone(),
            clients.reload.clone(),
            history.reload.clone(),
            listener.reload.clone(),
        ];
        Callback::from(move |_| {
            for reload in &reloads {
                reload.emit(());
            }
        })
    };
    let busy = devices.busy || certificates.busy || clients.busy || history.busy || listener.busy;
    use_refresh_action(reload.clone(), busy);

    // The live things are re-read on the timer: the connections, the
    // listener they hang off (which can bind while the page is open), and
    // the day's history when it is showing.
    {
        let (live, busy) = (*live, clients.busy || history.busy || listener.busy);
        let (clients, history, listener) = (
            clients.reload.clone(),
            history.reload.clone(),
            listener.reload.clone(),
        );
        let wide = !*connected_only;
        use_effect_with((live, busy, wide), move |(live, busy, wide)| {
            let wide = *wide;
            let handle = (*live && !*busy).then(|| {
                gloo_timers::callback::Timeout::new(REFRESH_MS, move || {
                    clients.emit(());
                    listener.emit(());
                    if wide {
                        history.emit(());
                    }
                })
            });
            move || drop(handle)
        });
    }

    let connected: &[ConnectedClient] = clients.data.as_deref().unwrap_or_default();
    let held = certificates.data.as_deref().unwrap_or_default();
    // Every connection the uid holds: the registry allows more than one.
    let connections_of = |device: &Device| -> Vec<ConnectedClient> {
        connected
            .iter()
            .filter(|client| client.client_uid == device.uid.to_string())
            .cloned()
            .collect()
    };

    let body = match (&devices.data, &devices.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the devices."
                message={message.clone()}
            />
        },
        (Some(list), _) => {
            let enrolled: Vec<(&Device, Vec<ConnectedClient>)> = list
                .iter()
                .filter(|device| matches_filter(device, &filter))
                .map(|device| (device, connections_of(device)))
                .filter(|(_, live)| !*connected_only || !live.is_empty())
                .collect();

            // Connected, but nothing here enrolled it.
            let unenrolled: Vec<&ConnectedClient> = connected
                .iter()
                .filter(|client| !list.iter().any(|d| d.uid.to_string() == client.client_uid))
                .filter(|client| {
                    let needle = filter.trim().to_lowercase();
                    needle.is_empty()
                        || [&client.callsign, &client.client_uid, &client.username]
                            .iter()
                            .any(|field| field.to_lowercase().contains(&needle))
                })
                .collect();

            if enrolled.is_empty() && unenrolled.is_empty() {
                html! {
                    <p class="panel-empty">
                        { if *connected_only {
                            empty_reason(&listener.data, !filter.trim().is_empty())
                        } else if list.is_empty() {
                            "Nothing has enrolled yet. Mint an enrolment token to bring a \
                             client on."
                        } else {
                            "No device matches that."
                        } }
                    </p>
                }
            } else {
                html! {
                    <ul class="device-list">
                        { for enrolled.into_iter().map(|(device, live)| html! {
                            <li key={device.id.get()}>
                                <DeviceRow
                                    device={device.clone()}
                                    certificate={certificate_for(device, held)}
                                    show_owner=true
                                    show_presence=true
                                    {live}
                                    on_changed={reload.clone()}
                                />
                            </li>
                        }) }
                        { for unenrolled.into_iter().map(|client| html! {
                            <li key={format!("live-{}", client.client_uid)}>
                                <ConnectedOnlyRow client={client.clone()} on_changed={reload.clone()} />
                            </li>
                        }) }
                    </ul>
                }
            }
        }
    };

    let actions = html! {
        <>
            <Switch
                id="euds-connected"
                checked={*connected_only}
                label="Connected only"
                onchange={
                    let connected_only = connected_only.clone();
                    Callback::from(move |on: bool| connected_only.set(on))
                }
            />
            <Switch
                id="euds-refresh"
                checked={*live}
                label="Auto-refresh"
                onchange={
                    let live = live.clone();
                    Callback::from(move |on: bool| live.set(on))
                }
            />
        </>
    };

    html! {
        <>
            <Card
                title="Enrolled devices"
                subtitle={if *connected_only {
                    "The devices holding a stream connection right now."
                } else {
                    "Everything that has enrolled, connected or not."
                }}
                {actions}
            >
                <Field label="Filter" id="device-filter" help="Matches callsign, identifier or owner.">
                    <TextInput
                        id="device-filter"
                        value={(*filter).clone()}
                        placeholder="QUINN"
                        onchange={
                            let filter = filter.clone();
                            Callback::from(move |value: String| filter.set(value))
                        }
                    />
                </Field>

                if let Some(message) = &clients.error {
                    <Alert
                        kind={AlertKind::Error}
                        title="We could not read what is connected."
                        message={message.clone()}
                    />
                }

                // How long the listener has been up: a short list reads very
                // differently against a listener that bound three days ago
                // and one that bound a minute ago.
                if let Some(bound_at) = listener.data.as_ref().and_then(|it| it.bound_at) {
                    <p class="panel-note" title={format_iso8601(bound_at)}>
                        { format!("The stream listener bound {}.", short_relative(bound_at)) }
                    </p>
                }

                { body }
            </Card>

            if !*connected_only {
                <Card
                    title="Recently disconnected"
                    subtitle="Seen in the last day, but not connected now."
                >
                    { history_list(&history.data, &history.error) }
                </Card>
            }
        </>
    }
}

fn history_list(data: &Option<Vec<ClientHistoryEntry>>, error: &Option<String>) -> Html {
    match (data, error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the client history."
                message={message.clone()}
            />
        },
        (Some(list), _) => {
            let gone: Vec<&ClientHistoryEntry> =
                list.iter().filter(|entry| !entry.connected).collect();

            if gone.is_empty() {
                html! {
                    <p class="panel-empty">
                        { if list.is_empty() {
                            "Nothing has connected in the last day."
                        } else {
                            "Everything seen in the last day is still connected."
                        } }
                    </p>
                }
            } else {
                html! {
                    <ul class="client-list">
                        { for gone.into_iter().map(|entry| html! {
                            <li key={entry.client_uid.clone()}>
                                <HistoryRow entry={entry.clone()} />
                            </li>
                        }) }
                    </ul>
                }
            }
        }
    }
}
