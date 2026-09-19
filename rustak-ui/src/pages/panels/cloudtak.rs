//! Onboarding CloudTAK: one button, and the three URLs plus two secrets it
//! produces.
//!
//! # Why this panel exists at all
//!
//! CloudTAK's *Configure Server* page will not save without an administrator
//! client certificate uploaded as a `.p12`, **and** a username and password for
//! the same account. Everywhere else rustak refuses to build a keystore,
//! because it never holds a device's private key — the Package tab beside this
//! one says so in as many words. CloudTAK cannot enrol, so this is the
//! exception, and the panel states it rather than hiding it.
//!
//! # The result replaces the form
//!
//! Both secrets are shown once and the download works once, so a page that
//! kept the button beside them would invite a second click that silently
//! replaces what is on screen with a hand-over nobody has collected. Asking
//! again is deliberate: the operator dismisses the result first.

use rustak_api::{CloudTakOnboarding, CloudTakOnboardingRequest, CloudTakPorts, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, NumberInput, TextInput,
};

use super::cloudtak_result::CloudTakResult;

#[derive(Properties, PartialEq)]
pub struct CloudTakPanelProps {
    /// The account CloudTAK will sign in as, and become system administrator
    /// of the first time it connects.
    pub username: Username,
}

#[function_component(CloudTakPanel)]
pub fn cloudtak_panel(props: &CloudTakPanelProps) -> Html {
    let host = use_state(String::new);
    let stream = use_state(|| None::<i64>);
    let marti = use_state(|| None::<i64>);
    let public = use_state(|| None::<i64>);
    let advanced = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let prepared = use_state(|| None::<CloudTakOnboarding>);

    let onboard = {
        let (host, stream, marti, public) =
            (host.clone(), stream.clone(), marti.clone(), public.clone());
        let (busy, error, prepared) = (busy.clone(), error.clone(), prepared.clone());
        let username = props.username.clone();

        Callback::from(move |_: MouseEvent| {
            let (busy, error, prepared) = (busy.clone(), error.clone(), prepared.clone());
            let username = username.clone();
            let request = CloudTakOnboardingRequest {
                host: trimmed(&host),
                ports: ports(*stream, *marti, *public),
                ..CloudTakOnboardingRequest::default()
            };

            busy.set(true);
            spawn_local(async move {
                match api::cloudtak::onboard(&username, &request).await {
                    Ok(onboarding) => {
                        error.set(None);
                        prepared.set(Some(onboarding));
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    if let Some(onboarding) = &*prepared {
        let dismiss = {
            let prepared = prepared.clone();
            Callback::from(move |_| prepared.set(None))
        };

        return html! {
            <CloudTakResult
                username={props.username.clone()}
                onboarding={onboarding.clone()}
                ondismiss={dismiss}
            />
        };
    }

    let footer = html! {
        <Button kind={ButtonKind::Primary} busy={*busy} onclick={onboard}>
            { "Onboard CloudTAK" }
        </Button>
    };

    html! {
        <Card
            title="Onboard CloudTAK"
            subtitle="Produces the certificate, the password and the three URLs CloudTAK's \
                      Configure Server page asks for."
            {footer}
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That hand-over could not be prepared."
                    message={message.clone()}
                />
            }

            <Alert
                kind={AlertKind::Warning}
                title="This is the one place rustak generates a client's private key."
                message="Everywhere else the device makes its own key and keeps it. CloudTAK \
                         cannot enrol, so the key is generated on the server for this one \
                         download, handed over once and discarded. It is written to the audit \
                         log, and the certificate can be revoked like any other."
            />

            <p class="panel-note">
                { "A client password is minted for this account and shown once. Nothing here \
                   is stored where it could be read back." }
            </p>

            <div class="stacked-actions">
                <button
                    type="button"
                    class="link-button"
                    onclick={
                        let advanced = advanced.clone();
                        Callback::from(move |_: MouseEvent| advanced.set(!*advanced))
                    }
                >
                    { if *advanced { "Hide host and ports" } else { "Override host and ports" } }
                </button>

                if *advanced {
                    <div class="inline-form">
                        <Field
                            label="Host"
                            id="cloudtak-host"
                            help="The name CloudTAK will reach this server by. Blank means the \
                                  one this installation calls itself."
                        >
                            <TextInput
                                id="cloudtak-host"
                                value={(*host).clone()}
                                placeholder="tak.example.com"
                                autocomplete="off"
                                onchange={
                                    let host = host.clone();
                                    Callback::from(move |value: String| host.set(value))
                                }
                            />
                        </Field>

                        { port_field("cloudtak-stream", "Stream port", 8089, &stream) }
                        { port_field("cloudtak-api", "Marti port", 8443, &marti) }
                        { port_field("cloudtak-webtak", "WebTAK port", 8446, &public) }
                    </div>
                }
            </div>
        </Card>
    }
}

/// One port override, which is empty until somebody has an opinion about it.
///
/// A published container is the reason this exists: rustak listens on 8089,
/// 8443 and 8446 inside and the operator forwards three other numbers outside,
/// and CloudTAK has to be told the outside ones.
fn port_field(
    id: &'static str,
    label: &'static str,
    default: i64,
    value: &UseStateHandle<Option<i64>>,
) -> Html {
    html! {
        <Field label={label} id={id} help={format!("Blank means {default}.")}>
            <NumberInput
                id={id}
                value={**value}
                min=1
                max=65535
                placeholder={default.to_string()}
                onchange={
                    let value = value.clone();
                    Callback::from(move |chosen: Option<i64>| value.set(chosen))
                }
            />
        </Field>
    }
}

/// An empty box means "use the configured one", not an empty host name.
fn trimmed(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_string())
}

/// The overrides, or nothing at all when none was typed.
fn ports(stream: Option<i64>, marti: Option<i64>, public: Option<i64>) -> Option<CloudTakPorts> {
    let ports = CloudTakPorts {
        stream: port(stream),
        marti: port(marti),
        public: port(public),
    };

    (!ports.is_empty()).then_some(ports)
}

/// A port number the browser will let somebody type, narrowed to one a URL can
/// carry. The field is bounded too; this is the one that cannot be bypassed.
fn port(value: Option<i64>) -> Option<u16> {
    value
        .and_then(|value| u16::try_from(value).ok())
        .filter(|port| *port > 0)
}
