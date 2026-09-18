//! Step five: the certificate authority.
//!
//! Its private key is generated on the server and sealed at rest; it never
//! leaves. Everything an EUD trusts hangs off this, so it is created once and
//! not replaced casually.
//!
//! # Why this step usually has nothing to decide
//!
//! The public listener presents a certificate issued by this authority, so the
//! server creates one during start-up — before it can bind, and therefore
//! before anybody can reach the wizard. On a normal installation the authority
//! already exists by the time this step is reached, and the honest thing to
//! show is what is there, with its fingerprint, rather than a form whose
//! answers would be ignored.
//!
//! The create form is still here for the case where there is nothing: an
//! installation whose listener does not need a certificate from us, or a
//! future in which `[pki]` can be switched off.

use rustak_api::{CaKeyType, CaSummary, InitCaRequest};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Field, LoadingNote, Select, SelectOption, TextInput,
};
use crate::util::{format_iso8601, urlencode};

#[derive(Properties, PartialEq)]
pub struct CaStepProps {
    pub on_created: Callback<()>,
}

#[function_component(CaStep)]
pub fn ca_step(props: &CaStepProps) -> Html {
    // `None` is "we have not asked yet"; `Some(None)` is "asked, and there is
    // nothing". Collapsing the two would show the create form for a moment on
    // every installation that already has an authority.
    let existing = use_state(|| None::<Option<CaSummary>>);
    let error = use_state(|| None::<String>);

    {
        let (existing, error) = (existing.clone(), error.clone());
        use_effect_with((), move |_| {
            spawn_local(async move {
                match api::setup::ca().await {
                    Ok(found) => existing.set(Some(found)),
                    // Not fatal: the step can still offer to create one, and
                    // the request that does is idempotent, so a server that
                    // has an authority we failed to read will hand it back.
                    Err(err) => {
                        error.set(Some(err.to_string()));
                        existing.set(Some(None));
                    }
                }
            });
            || ()
        });
    }

    let body = match &*existing {
        None => html! { <LoadingNote label="Looking for this server's authority…" /> },
        Some(Some(summary)) => html! {
            <Existing summary={summary.clone()} on_continue={props.on_created.clone()} />
        },
        Some(None) => html! { <Create on_created={props.on_created.clone()} /> },
    };

    html! {
        <>
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Warning}
                    title="We could not read this server's certificate authority."
                    message={message.clone()}
                />
            }
            { body }
        </>
    }
}

#[derive(Properties, PartialEq)]
struct ExistingProps {
    summary: CaSummary,
    on_continue: Callback<()>,
}

/// What is already there, and how to get hold of it.
#[function_component(Existing)]
fn existing(props: &ExistingProps) -> Html {
    let summary = &props.summary;
    let on_continue = {
        let on_continue = props.on_continue.clone();
        Callback::from(move |_: MouseEvent| on_continue.emit(()))
    };

    html! {
        <>
            <p class="wizard__lead">
                { "This server already has an authority — it made one at start-up, because \
                   the certificate it presents to your browser is issued by it. Everything \
                   rustak issues from now on is signed by this key, so check the fingerprint \
                   against the one in the server's log before you trust it anywhere." }
            </p>

            <dl class="detail-list">
                <dt>{ "Subject" }</dt>
                <dd>{ &summary.subject }</dd>

                <dt>{ "SHA-256 fingerprint" }</dt>
                <dd><code>{ &summary.fingerprint }</code></dd>

                <dt>{ "Valid from" }</dt>
                <dd>{ format_iso8601(summary.not_before) }</dd>

                <dt>{ "Valid until" }</dt>
                <dd>{ format_iso8601(summary.not_after) }</dd>
            </dl>

            if let Some(pem) = &summary.certificate_pem {
                <p class="wizard__lead">
                    <a
                        class="btn"
                        download="ca.crt"
                        href={format!("data:application/x-pem-file;charset=utf-8,{}", urlencode(pem))}
                    >
                        { "Download the authority certificate" }
                    </a>
                </p>
            }

            <Button kind={ButtonKind::Primary} onclick={on_continue}>{ "Continue" }</Button>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct CreateProps {
    on_created: Callback<()>,
}

/// The form, for an installation that genuinely has no authority yet.
#[function_component(Create)]
fn create(props: &CreateProps) -> Html {
    let common_name = use_state(|| "rustak CA".to_string());
    let organization = use_state(String::new);
    let key_type = use_state(CaKeyType::default);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_submit = {
        let (common_name, organization, key_type) =
            (common_name.clone(), organization.clone(), key_type.clone());
        let (busy, error, on_created) = (busy.clone(), error.clone(), props.on_created.clone());

        Callback::from(move |_: MouseEvent| {
            let request = InitCaRequest {
                common_name: common_name.trim().to_string(),
                organization: {
                    let trimmed = organization.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                },
                key_type: *key_type,
            };

            let (busy, error, on_created) = (busy.clone(), error.clone(), on_created.clone());
            busy.set(true);
            spawn_local(async move {
                match api::setup::init_ca(&request).await {
                    Ok(_) => {
                        error.set(None);
                        on_created.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let options: Vec<SelectOption> = CaKeyType::ALL
        .iter()
        .map(|kind| SelectOption::new(kind.as_str(), kind.label()))
        .collect();

    let chosen = AttrValue::from(key_type.as_str());
    let on_key_type = {
        let key_type = key_type.clone();
        Callback::from(move |value: Option<String>| {
            if let Some(parsed) = value.as_deref().and_then(CaKeyType::parse) {
                key_type.set(parsed);
            }
        })
    };

    html! {
        <>
            <p class="wizard__lead">
                { "This authority signs every client and server certificate rustak issues. \
                   Its key is generated here, sealed at rest, and never leaves the server." }
            </p>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="The certificate authority could not be created."
                    message={message.clone()}
                />
            }

            <Field id="ca-common-name" label="Common name" required=true
                help="What a device shows when it asks whether to trust this authority.">
                <TextInput
                    id="ca-common-name"
                    value={(*common_name).clone()}
                    onchange={let cn = common_name.clone(); Callback::from(move |v| cn.set(v))}
                />
            </Field>

            <Field id="ca-organization" label="Organisation">
                <TextInput
                    id="ca-organization"
                    value={(*organization).clone()}
                    placeholder="Example"
                    onchange={let org = organization.clone(); Callback::from(move |v| org.set(v))}
                />
            </Field>

            <Field id="ca-key-type" label="Key type"
                help="RSA is the compatible choice: older TAK clients and their Java \
                    keystores are reliable with it and less so with anything else.">
                <Select
                    id="ca-key-type"
                    value={Some(chosen)}
                    options={options}
                    onchange={on_key_type}
                />
            </Field>

            <Button
                kind={ButtonKind::Primary}
                busy={*busy}
                disabled={common_name.trim().is_empty()}
                onclick={on_submit}
            >
                { "Create the authority" }
            </Button>
        </>
    }
}
