//! One enrolled device: what it is, what it presented, whether it is here now,
//! and everything that can be done to it.
//!
//! The row is shared by the whole-installation EUDs page, by one account's
//! Devices tab and by a person's view of their own. Only the first of those
//! knows what is connected, so presence is a prop the others leave unset
//! rather than a pill that would read "Offline" on a page that never asked.
//!
//! # One button, four kinds of action
//!
//! Revoking the certificate is the face while there is one to revoke; behind
//! the caret come the reasoned revocations, then what can be done to a live
//! connection (incognito, disconnect), then forgetting the record. Each group
//! is separated because each has a different consequence: a revocation
//! refuses the next handshake, a disconnect is undone by reconnecting, and
//! forgetting loses what we knew while leaving the certificate working.

use rustak_api::{Certificate, ConnectedClient, Device, DeviceUid, RevocationReason};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{MenuAction, MenuItem, SplitButton, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative};

use super::certificate::{CertificateDetails, revoke_actions};

#[derive(Properties, PartialEq)]
pub struct DeviceRowProps {
    pub device: Device,

    /// The certificate it last presented, when one could be read.
    #[prop_or_default]
    pub certificate: Option<Certificate>,

    /// Whether to name the owner on the row.
    #[prop_or_default]
    pub show_owner: bool,

    /// Whether to say if the device is connected. A page that has not asked
    /// the stream registry leaves this unset rather than calling everything
    /// offline.
    #[prop_or_default]
    pub show_presence: bool,

    /// Its stream connection, when it holds one.
    #[prop_or_default]
    pub live: Option<ConnectedClient>,

    pub on_changed: Callback<()>,
}

#[function_component(DeviceRow)]
pub fn device_row(props: &DeviceRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // Every action on the row ends the same way: the flag comes down, the
    // failure (if any) goes under the row, and the lists are re-read.
    let finish = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        move |outcome: Result<(), api::ApiError>| {
            match outcome {
                Ok(()) => {
                    error.set(None);
                    on_changed.emit(());
                }
                Err(err) => error.set(Some(err.to_string())),
            }
            busy.set(false);
        }
    };

    let forget = {
        let (busy, finish) = (busy.clone(), finish.clone());
        let uid: DeviceUid = props.device.uid.clone();
        Callback::from(move |()| {
            let (busy, finish, uid) = (busy.clone(), finish.clone(), uid.clone());
            busy.set(true);
            spawn_local(async move { finish(api::devices::forget(&uid).await) });
        })
    };

    let revoke = {
        let (busy, finish) = (busy.clone(), finish.clone());
        let id = props.certificate.as_ref().map(|certificate| certificate.id);
        Callback::from(move |reason: RevocationReason| {
            let Some(id) = id else {
                return;
            };
            let (busy, finish) = (busy.clone(), finish.clone());
            busy.set(true);
            spawn_local(async move {
                finish(api::certificates::revoke(id, reason).await.map(|_| ()));
            });
        })
    };

    // `Some(on)` sets incognito; `None` disconnects.
    let act_live = {
        let (busy, finish) = (busy.clone(), finish.clone());
        let uid = props.device.uid.to_string();
        Callback::from(move |incognito: Option<bool>| {
            let (busy, finish, uid) = (busy.clone(), finish.clone(), uid.clone());
            busy.set(true);
            spawn_local(async move {
                let outcome = match incognito {
                    Some(on) => api::clients::set_incognito(&uid, on).await.map(|_| ()),
                    None => api::clients::disconnect(&uid).await,
                };
                finish(outcome);
            });
        })
    };

    let (primary, items) = actions(props, &revoke, &act_live, forget);

    html! {
        <div class="device-row">
            <div class="device-row__identity">
                <span class="device-row__name">{ props.device.display().to_string() }</span>
                <span class="device-row__uid">{ props.device.uid.to_string() }</span>
            </div>

            <div class="device-row__meta">
                if props.show_presence {
                    { presence(props.live.as_ref()) }
                }
                if props.show_owner {
                    <span title="Whose device this is">{ props.device.username.to_string() }</span>
                }
                if let Some(platform) = props.device.platform_version() {
                    <span>{ platform }</span>
                }
                if let Some(model) = &props.device.device_model {
                    <span>{ model.clone() }</span>
                }
                if let Some(client) = &props.live {
                    { live_details(client) }
                } else {
                    <span title={format_iso8601(props.device.last_seen_at)}>
                        { format!("Seen {}", short_relative(props.device.last_seen_at)) }
                    </span>
                    if let Some(ip) = props.device.last_ip {
                        <span title="Where we last saw it connect from">{ ip.to_string() }</span>
                    }
                }
            </div>

            <CertificateDetails
                certificate={props.certificate.clone()}
                had_one={props.device.last_certificate_id.is_some()}
            />

            <SplitButton
                busy={*busy}
                menu_label={format!("More actions for {}", props.device.display())}
                {primary}
                {items}
            />

            if let Some(message) = &*error {
                <p class="device-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

/// Whether it is here now, as a pill at the head of the metadata.
fn presence(live: Option<&ConnectedClient>) -> Html {
    match live {
        Some(client) => html! {
            <>
                <StatusPill tone={StatusTone::Ok} label="Connected" />
                if client.incognito {
                    <StatusPill
                        tone={StatusTone::Warning}
                        label="Incognito"
                        title="Its position is not forwarded to other clients."
                    />
                }
            </>
        },
        None => html! { <StatusPill tone={StatusTone::Neutral} label="Offline" /> },
    }
}

/// What a live connection says about itself.
fn live_details(client: &ConnectedClient) -> Html {
    let names = |held: &[rustak_api::GroupName]| match held.is_empty() {
        true => "none".to_string(),
        false => held
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    };

    html! {
        <>
            <span>{ format!("{} · {}", client.team, client.role) }</span>
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
                { format!("out: {} · in: {}", names(&client.out_groups), names(&client.in_groups)) }
            </span>
        </>
    }
}

/// The split button's face and menu for this row.
fn actions(
    props: &DeviceRowProps,
    revoke: &Callback<RevocationReason>,
    act_live: &Callback<Option<bool>>,
    forget: Callback<()>,
) -> (MenuAction, Vec<MenuItem>) {
    let name = props.device.display().to_string();

    let forget = MenuAction::new("Forget", forget).danger().confirm(
        format!(
            "Forget '{name}'? Its certificate stays valid — revoke the credential it enrolled \
             with to take that back.",
        ),
        "Forget it",
    );

    // What can be done to the connection, while there is one.
    let mut live_items = Vec::new();
    if let Some(client) = &props.live {
        let toggle = {
            let (act_live, on) = (act_live.clone(), !client.incognito);
            Callback::from(move |()| act_live.emit(Some(on)))
        };
        live_items.push(MenuItem::Action(MenuAction::new(
            if client.incognito {
                "Leave incognito"
            } else {
                "Go incognito"
            },
            toggle,
        )));

        let disconnect = {
            let act_live = act_live.clone();
            Callback::from(move |()| act_live.emit(None))
        };
        live_items.push(MenuItem::Action(
            MenuAction::new("Disconnect", disconnect).danger().confirm(
                format!(
                    "Disconnect '{name}'? It may reconnect immediately with the same \
                     certificate — revoke that to stop it.",
                ),
                "Disconnect it",
            ),
        ));
    }

    // Revoking is the face while there is a certificate to revoke; once there
    // is not, forgetting is all that is left of the device's own actions, and
    // it takes the face.
    match props
        .certificate
        .as_ref()
        .and_then(|certificate| revoke_actions(certificate, revoke))
    {
        Some((primary, mut items)) => {
            if !live_items.is_empty() {
                items.push(MenuItem::Separator);
                items.append(&mut live_items);
            }
            items.push(MenuItem::Separator);
            items.push(MenuItem::Action(forget));
            (primary, items)
        }
        None => (forget, live_items),
    }
}
