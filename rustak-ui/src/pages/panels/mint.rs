//! Minting a credential.
//!
//! Three kinds, and they are not equally good ideas. An enrolment token is the
//! one to reach for — one use, fifteen minutes, and it goes in a QR code. A
//! client password is a reusable secret that exists because CloudTAK needs one,
//! and the form says so rather than leaving somebody to find out. A service
//! token belongs to a sidecar and to nothing else.

use rustak_api::{CredentialCreated, CredentialKind, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, NumberInput, Select, SelectOption, TextInput,
};

use super::credentials::request;

#[derive(Properties, PartialEq)]
pub struct MintFormProps {
    /// Whose credential to mint. Absent mints for the signed-in account.
    #[prop_or_default]
    pub username: Option<Username>,

    pub on_minted: Callback<CredentialCreated>,
}

#[function_component(MintForm)]
pub fn mint_form(props: &MintFormProps) -> Html {
    let kind = use_state(|| CredentialKind::EnrollmentToken);
    let label = use_state(String::new);
    let days = use_state(|| None::<i64>);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_kind = {
        let kind = kind.clone();
        Callback::from(move |value: Option<String>| {
            if let Some(chosen) = value.as_deref().and_then(CredentialKind::parse) {
                kind.set(chosen);
            }
        })
    };

    let on_label = {
        let label = label.clone();
        Callback::from(move |value: String| label.set(value))
    };

    let on_days = {
        let days = days.clone();
        Callback::from(move |value: Option<i64>| days.set(value))
    };

    let submit = {
        let (kind, label, days) = (kind.clone(), label.clone(), days.clone());
        let (busy, error) = (busy.clone(), error.clone());
        let (username, on_minted) = (props.username.clone(), props.on_minted.clone());

        Callback::from(move |_: MouseEvent| {
            let (kind, label, days) = (kind.clone(), label.clone(), days.clone());
            let (busy, error) = (busy.clone(), error.clone());
            let (username, on_minted) = (username.clone(), on_minted.clone());

            let body = request(
                *kind,
                &label,
                username.as_ref(),
                days.and_then(|value| u32::try_from(value).ok()),
            );

            busy.set(true);
            spawn_local(async move {
                match api::credentials::create(&body).await {
                    Ok(created) => {
                        error.set(None);
                        label.set(String::new());
                        days.set(None);
                        on_minted.emit(created);
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let options: Vec<SelectOption> = CredentialKind::ALL
        .iter()
        .map(|kind| SelectOption::new(kind.as_str(), kind.label()))
        .collect();

    let footer = html! {
        <Button
            kind={ButtonKind::Primary}
            busy={*busy}
            disabled={label.trim().is_empty()}
            title={label.trim().is_empty().then_some("Give it a label first.")}
            onclick={submit}
        >
            { "Mint" }
        </Button>
    };

    html! {
        <Card
            title="Mint a credential"
            subtitle="Shown once, and only at the moment it is made."
            {footer}
        >
            if kind.is_compatibility_only() {
                <Alert
                    kind={AlertKind::Warning}
                    title="A client password is a compatibility credential."
                    message="It is a reusable secret that lasts ninety days, and it exists \
                        because CloudTAK cannot do anything better. It is accepted only by \
                        the password grant and by the enrolment endpoints — never for this \
                        console, the rest of the Marti API, or the stream. Prefer an \
                        enrolment token wherever the client can scan one."
                />
            }

            <div class="mint-form">
                <Field label="Kind" id="mint-kind" required=true>
                    <Select
                        id="mint-kind"
                        value={Some(AttrValue::from(kind.as_str()))}
                        options={options}
                        onchange={on_kind}
                    />
                </Field>

                <Field
                    label="Label"
                    id="mint-label"
                    required=true
                    help="Usually the name of the device it is for, so two can be told apart."
                >
                    <TextInput
                        id="mint-label"
                        value={(*label).clone()}
                        placeholder="Pixel 8"
                        onchange={on_label}
                    />
                </Field>

                <Field
                    label="Expires in (days)"
                    id="mint-days"
                    help={expiry_help(*kind)}
                >
                    <NumberInput id="mint-days" value={*days} min=1 max=3650 onchange={on_days} />
                </Field>
            </div>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That credential could not be minted."
                    message={message.clone()}
                />
            }
        </Card>
    }
}

/// What leaving the expiry blank means, which is different for each kind.
fn expiry_help(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::EnrollmentToken => {
            "Leave blank for the default of fifteen minutes. It is spent the first time it works."
        }
        CredentialKind::ClientPassword => "Leave blank for the default of ninety days.",
        CredentialKind::ServiceToken => {
            "Leave blank and it does not expire, which is what a sidecar usually wants."
        }
    }
}
