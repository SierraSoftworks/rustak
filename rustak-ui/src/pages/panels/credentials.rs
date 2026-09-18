//! The credentials one account holds, and the form that mints another.
//!
//! Shared by the administrator's view of somebody else's account and by a
//! person's view of their own, because the endpoint behind it is the same one:
//! `POST /api/v1/credentials` mints for the caller unless it is told a username,
//! and the server refuses that to anybody who is not an administrator. Writing
//! it once is what makes "enrol my own phone" and "enrol somebody's phone" the
//! same flow rather than two that could drift apart.

use rustak_api::{CreateCredentialRequest, Credential, CredentialId, CredentialKind, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Card, ConfirmButton, LoadingNote, QrCodeView, SecretReveal, StatusPill,
    StatusTone,
};
use crate::util::{format_iso8601, optional_relative};

use super::super::load::use_resource;
use super::mint::MintForm;

#[derive(Properties, PartialEq)]
pub struct CredentialsPanelProps {
    /// Whose credentials. Absent is the signed-in account's own, which is what
    /// the server answers when the request names nobody.
    #[prop_or_default]
    pub username: Option<Username>,

    /// The card's heading. See [`super::DevicesPanel`] for why a page passes
    /// something other than the default.
    #[prop_or(AttrValue::from("Credentials"))]
    pub title: AttrValue,
}

#[function_component(CredentialsPanel)]
pub fn credentials_panel(props: &CredentialsPanelProps) -> Html {
    let owner = props.username.clone();
    let credentials = use_resource(move || {
        let owner = owner.clone();
        async move { api::credentials::list(owner.as_ref(), true).await }
    });

    // What minting last returned. Held here rather than in the form so that the
    // list can reload underneath it without taking the secret off the screen.
    let minted = use_state(|| None::<rustak_api::CredentialCreated>);

    let on_minted = {
        let (minted, reload) = (minted.clone(), credentials.reload.clone());
        Callback::from(move |created: rustak_api::CredentialCreated| {
            minted.set(Some(created));
            reload.emit(());
        })
    };

    let dismiss = {
        let minted = minted.clone();
        Callback::from(move |_| minted.set(None))
    };

    let body = match (&credentials.data, &credentials.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the credentials."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "No credentials yet. Mint an enrolment token to bring a client on." }
            </p>
        },
        (Some(list), _) => html! {
            <ul class="credential-list">
                { for list.iter().map(|credential| html! {
                    <li key={credential.id.get()}>
                        <CredentialRow
                            credential={credential.clone()}
                            on_changed={credentials.reload.clone()}
                        />
                    </li>
                }) }
            </ul>
        },
    };

    html! {
        <>
            if let Some(created) = &*minted {
                <Revealed created={created.clone()} ondismiss={dismiss} />
            }

            <MintForm username={props.username.clone()} {on_minted} />

            <Card title={props.title.clone()} subtitle="Everything this account can present.">
                { body }
            </Card>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct RevealedProps {
    created: rustak_api::CredentialCreated,
    ondismiss: Callback<()>,
}

/// The one moment the secret exists outside the client that will use it.
#[function_component(Revealed)]
fn revealed(props: &RevealedProps) -> Html {
    let created = &props.created;

    html! {
        <SecretReveal
            title={format!("{} — {}", created.credential.kind.label(), created.credential.label)}
            secret={created.secret.clone()}
            enroll_url={created.enroll_url.clone().map(AttrValue::from)}
            ondismiss={props.ondismiss.clone()}
        >
            if let Some(url) = &created.enroll_url {
                <div class="secret-reveal__qr">
                    <QrCodeView data={url.clone()} alt="Enrolment QR code for ATAK" />
                    <p class="secret-reveal__qr-note">
                        { "In ATAK, choose Settings → Network Preferences → Manage Server \
                           Connections → Quick Connect and scan this." }
                    </p>
                </div>
            }
        </SecretReveal>
    }
}

#[derive(Properties, PartialEq)]
struct CredentialRowProps {
    credential: Credential,
    on_changed: Callback<()>,
}

#[function_component(CredentialRow)]
fn credential_row(props: &CredentialRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let revoke = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let id: CredentialId = props.credential.id;
        Callback::from(move |_| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::credentials::revoke(id).await {
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

    let (tone, status) = state_of(&props.credential);
    let gone = props.credential.is_revoked() || props.credential.is_exhausted();

    html! {
        <div class="credential-row">
            <div class="credential-row__identity">
                <span class="credential-row__label">{ props.credential.label.clone() }</span>
                <span class="credential-row__kind">{ props.credential.kind.label() }</span>
            </div>

            <div class="credential-row__meta">
                <span title="When it stops working">{ expiry(&props.credential) }</span>
                <span title="How many times it has been used">{ uses(&props.credential) }</span>
                <span title="When it was last presented">
                    { format!("Last used {}", optional_relative(props.credential.last_used_at)) }
                </span>
            </div>

            <StatusPill {tone} label={status} />

            <ConfirmButton
                label="Revoke"
                confirm_label="Revoke it"
                question={format!(
                    "Revoke '{}'? Any certificate issued with it is revoked too.",
                    props.credential.label,
                )}
                disabled={gone}
                busy={*busy}
                title={gone.then_some("This credential has already gone.")}
                onconfirm={revoke}
            />

            if let Some(message) = &*error {
                <p class="credential-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

/// How a credential is doing, in the two or three words a pill has room for.
fn state_of(credential: &Credential) -> (StatusTone, &'static str) {
    if credential.is_revoked() {
        (StatusTone::Neutral, "Revoked")
    } else if credential.is_exhausted() {
        (StatusTone::Neutral, "Spent")
    } else if !credential.is_usable_at(chrono::Utc::now()) {
        (StatusTone::Warning, "Expired")
    } else {
        (StatusTone::Ok, "Active")
    }
}

fn expiry(credential: &Credential) -> String {
    match credential.expires_at {
        Some(at) => format!(
            "Expires {} ({})",
            optional_relative(Some(at)),
            format_iso8601(at)
        ),
        None => "Does not expire".to_string(),
    }
}

fn uses(credential: &Credential) -> String {
    match credential.max_uses {
        Some(max) => format!("{} of {max} uses", credential.uses),
        None => format!("{} uses", credential.uses),
    }
}

/// The request the form builds, kept here so the form and the panel agree on
/// what a blank field means.
pub fn request(
    kind: CredentialKind,
    label: &str,
    username: Option<&Username>,
    expires_in_days: Option<u32>,
) -> CreateCredentialRequest {
    CreateCredentialRequest {
        kind,
        label: label.trim().to_string(),
        username: username.cloned(),
        expires_in_days,
        // Absent means the default for the kind, which is once for an enrolment
        // token — and choosing a number here would only be a way to weaken that.
        max_uses: None,
    }
}
