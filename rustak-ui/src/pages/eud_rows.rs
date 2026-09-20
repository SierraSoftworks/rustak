//! The rows on the EUDs page that are not enrolled devices: a connection
//! nothing here enrolled, and a device seen today but gone now.

use rustak_api::{ClientHistoryEntry, ConnectedClient, StreamStatus};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{MenuAction, MenuItem, SplitButton, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative};

use super::panels::live_details;

/// Why the connected list is empty, once the listener has said whether it is
/// running. Until it has, the page says the ambiguous thing, which is honest
/// for the moment before the answer arrives.
pub fn empty_reason(listener: &Option<StreamStatus>, filtered: bool) -> &'static str {
    if filtered {
        return "Nothing connected matches that.";
    }

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

#[derive(Properties, PartialEq)]
pub struct ConnectedOnlyRowProps {
    pub client: ConnectedClient,
    pub on_changed: Callback<()>,
}

/// A connection with no enrolled device behind it: a certificate this
/// installation did not issue, or a record that was forgotten while the
/// socket stayed open. Either way the only levers are on the connection.
#[function_component(ConnectedOnlyRow)]
pub fn connected_only_row(props: &ConnectedOnlyRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let client = &props.client;

    // `Some(on)` sets incognito; `None` disconnects.
    let act = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let uid = client.client_uid.clone();
        Callback::from(move |incognito: Option<bool>| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let uid = uid.clone();
            busy.set(true);
            spawn_local(async move {
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

    let disconnect = MenuAction::new("Disconnect", {
        let act = act.clone();
        Callback::from(move |()| act.emit(None))
    })
    .danger()
    .confirm(
        format!(
            "Disconnect '{}'? It may reconnect immediately with the same certificate.",
            client.callsign,
        ),
        "Disconnect it",
    );
    let toggle = MenuAction::new(
        if client.incognito {
            "Leave incognito"
        } else {
            "Go incognito"
        },
        {
            let (act, on) = (act.clone(), !client.incognito);
            Callback::from(move |()| act.emit(Some(on)))
        },
    );

    html! {
        <div class="device-row">
            <div class="device-row__identity">
                <span class="device-row__name">{ client.callsign.clone() }</span>
                <span class="device-row__uid">{ client.client_uid.clone() }</span>
            </div>

            <div class="device-row__meta">
                <StatusPill tone={StatusTone::Ok} label="Connected" />
                <StatusPill
                    tone={StatusTone::Warning}
                    label="Not enrolled"
                    title="Connected with a certificate this installation has no device record for."
                />
                <span title="The account it presented as">{ client.username.clone() }</span>
                <span>{ client.takv.clone() }</span>
                { live_details(client) }
            </div>

            <span class="certificate">{ "No device record" }</span>

            <SplitButton
                busy={*busy}
                menu_label={format!("More actions for {}", client.callsign)}
                primary={disconnect}
                items={vec![MenuItem::Action(toggle)]}
            />

            if let Some(message) = &*error {
                <p class="device-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct HistoryRowProps {
    pub entry: ClientHistoryEntry,
}

/// A device seen in the window but not connected now.
#[function_component(HistoryRow)]
pub fn history_row(props: &HistoryRowProps) -> Html {
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

            <StatusPill tone={StatusTone::Neutral} label="Offline" />
        </div>
    }
}
