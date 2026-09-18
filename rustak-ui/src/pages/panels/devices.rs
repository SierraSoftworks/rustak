//! The clients that have enrolled, and what they came as.
//!
//! Shared by the administrator's whole-installation list, by one account's
//! Devices tab, and by a person's view of their own — `GET /api/v1/devices`
//! narrows by `username` and answers an administrator with everything when it is
//! left off, so the three are one component with one prop between them.
//!
//! # Forgetting is not revoking
//!
//! `DELETE /api/v1/devices/{uid}` removes what we knew about a client; the
//! certificate it enrolled with belongs to the *account*, and taking that back
//! is what revoking the credential it was issued against does. The button says
//! "Forget" for that reason, and the panel points at the credential list rather
//! than implying a revocation it does not perform.

use rustak_api::{Device, DeviceUid, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, ConfirmButton, Field, LoadingNote, TextInput};
use crate::util::{format_iso8601, short_relative};

use super::super::load::use_resource;

#[derive(Properties, PartialEq)]
pub struct DevicesPanelProps {
    /// Whose devices. Absent lists every device in the installation for an
    /// administrator, and the caller's own for anybody else.
    #[prop_or_default]
    pub username: Option<Username>,

    /// Offers a box that narrows the list by callsign, uid or owner. Worth it on
    /// the whole-installation page and noise on one account's tab.
    #[prop_or_default]
    pub filterable: bool,

    /// Whether to name the owner on each row.
    #[prop_or_default]
    pub show_owner: bool,

    /// The card's heading. A page whose own title is already "Devices" passes
    /// something else, so the reader is not told the same word twice and a
    /// screen reader does not announce two identical headings.
    #[prop_or(AttrValue::from("Devices"))]
    pub title: AttrValue,
}

#[function_component(DevicesPanel)]
pub fn devices_panel(props: &DevicesPanelProps) -> Html {
    let owner = props.username.clone();
    let devices = use_resource(move || {
        let owner = owner.clone();
        async move { api::devices::list(owner.as_ref()).await }
    });

    let filter = use_state(String::new);
    let on_filter = {
        let filter = filter.clone();
        Callback::from(move |value: String| filter.set(value))
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
            let matching: Vec<&Device> = list
                .iter()
                .filter(|device| matches_filter(device, &filter))
                .collect();

            if matching.is_empty() {
                html! {
                    <p class="panel-empty">
                        { if list.is_empty() {
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
                        { for matching.into_iter().map(|device| html! {
                            <li key={device.id.get()}>
                                <DeviceRow
                                    device={device.clone()}
                                    show_owner={props.show_owner}
                                    on_changed={devices.reload.clone()}
                                />
                            </li>
                        }) }
                    </ul>
                }
            }
        }
    };

    html! {
        <Card title={props.title.clone()} subtitle="What has connected, and what it came as.">
            if props.filterable {
                <Field label="Filter" id="device-filter" help="Matches callsign, identifier or owner.">
                    <TextInput
                        id="device-filter"
                        value={(*filter).clone()}
                        placeholder="QUINN"
                        onchange={on_filter}
                    />
                </Field>
            }
            { body }
        </Card>
    }
}

/// Whether a device matches what somebody typed, ignoring case.
fn matches_filter(device: &Device, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }

    [
        device.callsign.clone().unwrap_or_default(),
        device.uid.to_string(),
        device.username.to_string(),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(&needle))
}

#[derive(Properties, PartialEq)]
struct DeviceRowProps {
    device: Device,
    show_owner: bool,
    on_changed: Callback<()>,
}

#[function_component(DeviceRow)]
fn device_row(props: &DeviceRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let forget = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let uid: DeviceUid = props.device.uid.clone();
        Callback::from(move |_| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let uid = uid.clone();
            busy.set(true);
            spawn_local(async move {
                match api::devices::forget(&uid).await {
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
        <div class="device-row">
            <div class="device-row__identity">
                <span class="device-row__name">{ props.device.display().to_string() }</span>
                <span class="device-row__uid">{ props.device.uid.to_string() }</span>
            </div>

            <div class="device-row__meta">
                if props.show_owner {
                    <span title="Whose device this is">{ props.device.username.to_string() }</span>
                }
                if let Some(platform) = props.device.platform_version() {
                    <span>{ platform }</span>
                }
                if let Some(model) = &props.device.device_model {
                    <span>{ model.clone() }</span>
                }
                <span title={format_iso8601(props.device.last_seen_at)}>
                    { format!("Seen {}", short_relative(props.device.last_seen_at)) }
                </span>
                if let Some(ip) = props.device.last_ip {
                    <span title="Where we last saw it connect from">{ ip.to_string() }</span>
                }
                <span title={certificate_title(&props.device)}>{ certificate(&props.device) }</span>
            </div>

            <ConfirmButton
                label="Forget"
                confirm_label="Forget it"
                question={format!(
                    "Forget '{}'? Its certificate stays valid — revoke the credential it \
                     enrolled with to take that back.",
                    props.device.display(),
                )}
                busy={*busy}
                onconfirm={forget}
            />

            if let Some(message) = &*error {
                <p class="device-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

/// What we can say about the certificate this device presented.
///
/// Only that there was one: the admin API has no endpoint that describes a
/// certificate, so the fingerprint and the expiry an operator wants here cannot
/// be read yet (see the status file for M2-07).
fn certificate(device: &Device) -> String {
    match device.last_certificate_id {
        Some(id) => format!("Certificate #{id}"),
        None => "No certificate".to_string(),
    }
}

fn certificate_title(device: &Device) -> &'static str {
    match device.last_certificate_id {
        Some(_) => "The certificate this device last presented.",
        None => "This device has never presented a client certificate.",
    }
}
