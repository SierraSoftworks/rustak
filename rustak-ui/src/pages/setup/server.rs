//! Two of the last three steps: what the server is called, and closing the
//! wizard. The authority between them is [`super::ca`], which is a step with
//! two quite different faces.

use rustak_api::ServerSettingsRequest;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Field, TextInput};
use crate::util::nav_href;

#[derive(Properties, PartialEq)]
pub struct ServerStepProps {
    pub on_saved: Callback<()>,
}

/// Step four: the host names this server is reached on.
///
/// They matter more than they look: they become the subject alternative names of
/// the server certificate, the host in every enrolment QR code, and the WebAuthn
/// relying-party identifier. Getting them wrong means devices that enrol and
/// then cannot connect.
#[function_component(ServerStep)]
pub fn server_step(props: &ServerStepProps) -> Html {
    let name = use_state(String::new);
    let domains = use_state(String::new);
    let base_url = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_submit = {
        let (name, domains, base_url) = (name.clone(), domains.clone(), base_url.clone());
        let (busy, error, on_saved) = (busy.clone(), error.clone(), props.on_saved.clone());

        Callback::from(move |_: MouseEvent| {
            let request = ServerSettingsRequest {
                name: name.trim().to_string(),
                domains: domains
                    .split(',')
                    .map(str::trim)
                    .filter(|domain| !domain.is_empty())
                    .map(str::to_string)
                    .collect(),
                base_url: {
                    let trimmed = base_url.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                },
            };

            let (busy, error, on_saved) = (busy.clone(), error.clone(), on_saved.clone());
            busy.set(true);
            spawn_local(async move {
                match api::setup::set_server(&request).await {
                    Ok(_) => {
                        error.set(None);
                        on_saved.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    html! {
        <>
            <p class="wizard__lead">
                { "Clients are told this when they connect, and the server certificate is \
                   issued for it." }
            </p>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="The server settings could not be saved."
                    message={message.clone()}
                />
            }

            <Field id="server-name" label="Server name" required=true
                help="What this installation is called in client server lists.">
                <TextInput
                    id="server-name"
                    value={(*name).clone()}
                    placeholder="Example TAK"
                    onchange={let name = name.clone(); Callback::from(move |v| name.set(v))}
                />
            </Field>

            <Field id="server-domains" label="Host names" required=true
                help="Comma separated, canonical first. These become the certificate's \
                    subject alternative names.">
                <TextInput
                    id="server-domains"
                    value={(*domains).clone()}
                    placeholder="tak.example.com, 203.0.113.24"
                    onchange={let domains = domains.clone(); Callback::from(move |v| domains.set(v))}
                />
            </Field>

            <Field id="server-base-url" label="Base URL"
                help="Only needed when it cannot be derived from the host name and the \
                    listener's port — behind a reverse proxy, for instance.">
                <TextInput
                    id="server-base-url"
                    value={(*base_url).clone()}
                    placeholder="https://tak.example.com:8446"
                    onchange={let base_url = base_url.clone(); Callback::from(move |v| base_url.set(v))}
                />
            </Field>

            <Button
                kind={ButtonKind::Primary}
                busy={*busy}
                disabled={name.trim().is_empty() || domains.trim().is_empty()}
                onclick={on_submit}
            >
                { "Save and continue" }
            </Button>
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct DoneStepProps {
    /// Whether the wizard has already been closed, in which case there is
    /// nothing left to do but leave.
    pub completed: bool,
    pub on_complete: Callback<()>,
}

/// Step six: closing the wizard, which is a one-way door.
#[function_component(DoneStep)]
pub fn done_step(props: &DoneStepProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_finish = {
        let (busy, error, on_complete) = (busy.clone(), error.clone(), props.on_complete.clone());
        Callback::from(move |_: MouseEvent| {
            let (busy, error, on_complete) = (busy.clone(), error.clone(), on_complete.clone());
            busy.set(true);
            spawn_local(async move {
                match api::setup::complete().await {
                    Ok(()) => {
                        error.set(None);
                        on_complete.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    if props.completed {
        return html! {
            <>
                <Alert
                    kind={AlertKind::Success}
                    title="This server is set up"
                    message="The wizard is closed: its routes now answer 410 for good, so it \
                        cannot be used to take this installation over."
                />
                <a class="btn btn--primary btn--lg" href={nav_href("/admin")}>
                    { "Open the console" }
                </a>
            </>
        };
    }

    html! {
        <>
            <p class="wizard__lead">
                { "That is everything. Finishing closes the wizard permanently — its routes \
                   answer 410 from then on, so nobody can walk it again to take the \
                   installation over." }
            </p>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="The wizard could not be closed."
                    message={message.clone()}
                />
            }

            <Button kind={ButtonKind::Primary} busy={*busy} onclick={on_finish}>
                { "Finish setup" }
            </Button>
        </>
    }
}
