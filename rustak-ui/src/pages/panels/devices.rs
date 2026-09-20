//! The clients that have enrolled, and what they came as.
//!
//! Shared by the administrator's whole-installation list, by one account's
//! Devices tab, and by a person's view of their own — `GET /api/v1/devices`
//! narrows by `username` and answers an administrator with everything when it is
//! left off, so the three are one component with one prop between them.
//!
//! The row itself is [`super::device_row`], which the EUDs page also uses
//! with the connection registry joined on.

use rustak_api::{Certificate, Device, Username};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, Field, LoadingNote, TextInput};

use super::super::load::use_resource;
use super::device_row::DeviceRow;

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
pub fn matches_filter(device: &Device, needle: &str) -> bool {
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
pub fn certificate_for(device: &Device, certificates: &[Certificate]) -> Option<Certificate> {
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
