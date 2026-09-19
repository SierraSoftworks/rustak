//! The clients that have enrolled, and what they came as.
//!
//! Shared by the administrator's whole-installation list, by one account's
//! Devices tab, and by a person's view of their own — `GET /api/v1/devices`
//! narrows by `username` and answers an administrator with everything when it is
//! left off, so the three are one component with one prop between them.
//!
//! # Forgetting is not revoking
//!
//! `DELETE /api/v1/devices/{uid}` removes what we knew about a client and
//! leaves its certificate working, so the button says "Forget". Taking the
//! certificate back is the *other* action on the row — see
//! [`super::certificate`] — and it is separate because the two have different
//! consequences: forgetting loses a record, revoking drops a live connection
//! and refuses the next handshake. Both sit on one split button, the revocation
//! on its face and the rest behind the caret, with a rule between the two kinds
//! of thing.

use rustak_api::{Certificate, Device, DeviceUid, RevocationReason, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Card, Field, LoadingNote, MenuAction, MenuItem, SplitButton, TextInput,
};
use crate::util::{format_iso8601, short_relative};

use super::super::load::use_resource;
use super::certificate::{CertificateDetails, revoke_actions};

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

    // One request for the whole list rather than one per row: a device names
    // the certificate it last presented by identifier, and resolving each one
    // on its own would be an N+1 on a page whose whole job is to be scanned.
    let owner = props.username.clone();
    let certificates = use_resource(move || {
        let owner = owner.clone();
        async move { api::certificates::list(owner.as_ref(), None).await }
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
                                    certificate={certificate_for(
                                        device,
                                        certificates.data.as_deref().unwrap_or_default(),
                                    )}
                                    show_owner={props.show_owner}
                                    on_changed={
                                        let (devices, certificates) =
                                            (devices.reload.clone(), certificates.reload.clone());
                                        Callback::from(move |_| {
                                            devices.emit(());
                                            certificates.emit(());
                                        })
                                    }
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

/// The certificate this device last presented, out of the ones we read.
///
/// By identifier first, because that is what the device row actually points
/// at. The fallback to the newest one issued to the same uid covers a device
/// whose row has not caught up with a renewal — showing the certificate that
/// is really in use beats showing none.
fn certificate_for(device: &Device, certificates: &[Certificate]) -> Option<Certificate> {
    if let Some(found) = device
        .last_certificate_id
        .and_then(|id| certificates.iter().find(|held| held.id == id))
    {
        return Some(found.clone());
    }

    certificates
        .iter()
        .filter(|held| held.device_uid.as_ref() == Some(&device.uid))
        .max_by_key(|held| held.not_before)
        .cloned()
}

#[derive(Properties, PartialEq)]
struct DeviceRowProps {
    device: Device,

    /// The certificate it last presented, when one could be read.
    #[prop_or_default]
    certificate: Option<Certificate>,

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
        Callback::from(move |()| {
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

    let revoke = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let id = props.certificate.as_ref().map(|certificate| certificate.id);
        Callback::from(move |reason: RevocationReason| {
            let Some(id) = id else {
                return;
            };
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::certificates::revoke(id, reason).await {
                    Ok(_) => {
                        error.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let forget = MenuAction::new("Forget", forget).danger().confirm(
        format!(
            "Forget '{}'? Its certificate stays valid — revoke the credential it enrolled \
             with to take that back.",
            props.device.display(),
        ),
        "Forget it",
    );

    // Revoking is the face of the button while there is a certificate to
    // revoke; once there is not, forgetting is all that is left, and it takes
    // the face with no caret beside it.
    let (primary, items) = match props
        .certificate
        .as_ref()
        .and_then(|c| revoke_actions(c, &revoke))
    {
        Some((primary, mut items)) => {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Action(forget));
            (primary, items)
        }
        None => (forget, Vec::new()),
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
