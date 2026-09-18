//! The configuration package an operator sends somebody setting a client up
//! by hand.
//!
//! Enrolment is the path we want people on — a one-time token, a
//! device-generated key, a certificate that never leaves the device. This is
//! the fallback for a client that cannot enrol, and it is deliberately narrow.
//!
//! # No keystore, and the reason is not a limitation to work around
//!
//! rustak never holds a device's private key: a certificate is issued against
//! a signing request the device made, and the key stays there. There is
//! nothing to assemble a `.p12` keystore out of after the fact, and the only
//! way to offer one would be to mint a key pair on the server — which is the
//! thing the whole enrolment design exists to avoid. So the package carries
//! the truststore and the connection, and the client enrols for its own
//! certificate on first connect.
//!
//! # The credential is named, never minted
//!
//! Naming one asserts that a live client password exists for the person to
//! type at the enrolment prompt. Minting one as a side effect of a download
//! would put a long-lived secret into a file whose only job is to be emailed
//! around, and nothing would ever revoke it.

use rustak_api::{
    ConfigPackageRequest, ConfigPackageVariant, Credential, CredentialKind, Username,
};
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, LoadingNote, Select, SelectOption,
};

use super::super::load::{use_download, use_resource};

#[derive(Properties, PartialEq)]
pub struct ConfigPackagePanelProps {
    /// The account the package configures.
    pub username: Username,
}

/// Whether a credential is one somebody could type at an enrolment prompt.
fn is_usable(credential: &Credential) -> bool {
    credential.kind == CredentialKind::ClientPassword && credential.revoked_at.is_none()
}

#[function_component(ConfigPackagePanel)]
pub fn config_package_panel(props: &ConfigPackagePanelProps) -> Html {
    let owner = props.username.clone();
    let credentials = use_resource(move || {
        let owner = owner.clone();
        async move { api::credentials::list(Some(&owner), false).await }
    });

    let variant = use_state(ConfigPackageVariant::default);
    let credential = use_state(|| None::<String>);

    let request = ConfigPackageRequest {
        username: props.username.clone(),
        credential_id: credential
            .as_deref()
            .and_then(|id| id.parse::<i64>().ok())
            .map(rustak_api::CredentialId::new),
        variant: *variant,
        // Never offered: see the module documentation.
        include_client_cert: false,
    };

    let download = {
        let request = request.clone();
        use_download(move || {
            let request = request.clone();
            async move { api::config_packages::create(&request).await }
        })
    };

    let usable: Vec<&Credential> = credentials
        .data
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|credential| is_usable(credential))
        .collect();

    let options: Vec<SelectOption> = usable
        .iter()
        .map(|credential| {
            SelectOption::new(credential.id.get().to_string(), credential.label.clone())
        })
        .collect();

    let variants: Vec<SelectOption> = ConfigPackageVariant::ALL
        .iter()
        .map(|variant| SelectOption::new(variant.as_str(), variant.label()))
        .collect();

    html! {
        <Card
            title="Configuration package"
            subtitle="For a client that cannot enrol. Import it to end up with this server \
                      configured."
        >
            if let Some(message) = &download.error {
                <Alert
                    kind={AlertKind::Error}
                    title="That package could not be built."
                    message={message.clone()}
                />
            }

            <Field
                label="Client"
                id="package-variant"
                help="ATAK and WinTAK take a package inside a package; iTAK takes a flat zip."
            >
                <Select
                    id="package-variant"
                    value={Some(AttrValue::from(variant.as_str()))}
                    options={variants}
                    onchange={
                        let variant = variant.clone();
                        Callback::from(move |chosen: Option<String>| {
                            if let Some(chosen) =
                                chosen.as_deref().and_then(ConfigPackageVariant::parse)
                            {
                                variant.set(chosen);
                            }
                        })
                    }
                />
            </Field>

            if credentials.busy && credentials.data.is_none() {
                <LoadingNote label="Loading credentials…" />
            } else {
                <Field
                    label="Client password"
                    id="package-credential"
                    help="The one they will type at the enrolment prompt. The secret itself is \
                          never in the package."
                >
                    <Select
                        id="package-credential"
                        value={(*credential).clone().map(AttrValue::from)}
                        options={options}
                        clearable=true
                        placeholder="None — enrol with a token instead"
                        disabled={usable.is_empty()}
                        onchange={
                            let credential = credential.clone();
                            Callback::from(move |chosen: Option<String>| credential.set(chosen))
                        }
                    />
                </Field>
            }

            if usable.is_empty() && !credentials.busy {
                <p class="panel-empty">
                    { "This account holds no live client password. The package still works — \
                       the device enrols with a one-time token instead — but mint one on the \
                       Credentials tab if they are to connect by name and password." }
                </p>
            }

            <Alert
                kind={AlertKind::Info}
                title="No keystore travels in this package."
                message="rustak never holds a device's private key, so there is nothing to put \
                         in one. The package carries the truststore and the connection, and the \
                         client enrols for its own certificate on first connect."
            />

            <Button
                kind={ButtonKind::Primary}
                busy={download.busy}
                onclick={
                    let start = download.start.clone();
                    Callback::from(move |_: MouseEvent| start.emit(()))
                }
            >
                { "Download package" }
            </Button>
        </Card>
    }
}
