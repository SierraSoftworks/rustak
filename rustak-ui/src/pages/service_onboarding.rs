//! "Add a service": one action that sets a sidecar deployment up, and the one
//! view of what it produced.
//!
//! Deploying a sidecar without an orchestrator identity takes a service
//! account, a service token and a one-time enrolment token — and the enrolment
//! token expires minutes after it is minted. This card asks for the name and
//! answers with everything the deployment pastes in: the two secrets, the
//! `[service]` fragment and the environment lines, each with a copy button,
//! and the expiry stated beside them.
//!
//! # The result replaces the form
//!
//! As on the CloudTAK panel: the secrets are shown once, so a form left beside
//! them would invite a second click that replaces what is on screen with a
//! hand-over nobody has copied. Asking again is deliberate — the operator
//! dismisses the result first.

use rustak_api::{ServiceOnboarding, ServiceOnboardingRequest, ServiceTokenOutcome};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Copyable, Field, Switch, TextInput,
};
use crate::util::format_iso8601;

/// The request the form describes: a blank account means the service's own
/// name, which is the server's default too.
pub fn request(name: &str, account: &str, rotate: bool) -> ServiceOnboardingRequest {
    ServiceOnboardingRequest {
        name: name.trim().to_string(),
        account: (!account.trim().is_empty()).then(|| account.trim().to_string()),
        rotate_service_token: rotate,
    }
}

/// What the service-token line of the result says.
pub fn token_summary(onboarding: &ServiceOnboarding) -> &'static str {
    match onboarding.service_token_outcome {
        ServiceTokenOutcome::Minted => "A service token was minted for the control API.",
        ServiceTokenOutcome::Kept => {
            "The account's existing service token was kept and is not shown again: the \
             deployment keeps the one it has."
        }
        ServiceTokenOutcome::Rotated => {
            "A new service token was minted and the old one revoked: whatever still uses the \
             old one can no longer reach the control API."
        }
    }
}

#[function_component(AddService)]
pub fn add_service() -> Html {
    let name = use_state(String::new);
    let account = use_state(String::new);
    let rotate = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let prepared = use_state(|| None::<ServiceOnboarding>);

    let submit = {
        let (name, account, rotate) = (name.clone(), account.clone(), rotate.clone());
        let (busy, error, prepared) = (busy.clone(), error.clone(), prepared.clone());

        Callback::from(move |_: MouseEvent| {
            let (busy, error, prepared) = (busy.clone(), error.clone(), prepared.clone());
            let request = request(&name, &account, *rotate);

            busy.set(true);
            spawn_local(async move {
                match api::service_onboarding::onboard(&request).await {
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
            let (prepared, name, account, rotate) = (
                prepared.clone(),
                name.clone(),
                account.clone(),
                rotate.clone(),
            );
            Callback::from(move |_: MouseEvent| {
                prepared.set(None);
                name.set(String::new());
                account.set(String::new());
                rotate.set(false);
            })
        };

        return result(onboarding, dismiss);
    }

    let actions = html! {
        <Button
            small=true
            kind={ButtonKind::Primary}
            busy={*busy}
            disabled={name.trim().is_empty()}
            onclick={submit}
        >
            { "Add service" }
        </Button>
    };

    html! {
        <Card
            title="Add a service"
            subtitle="Creates the sidecar's account and mints what its deployment needs, in one \
                      step. Every secret is shown once."
            {actions}
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That service could not be added."
                    message={message.clone()}
                />
            }

            <div class="inline-form">
                <Field
                    label="Service name"
                    id="service-name"
                    required=true
                    help="What [service] name will say: lower-case letters, digits and hyphens."
                >
                    <TextInput
                        id="service-name"
                        value={(*name).clone()}
                        placeholder="adsb"
                        autocomplete="off"
                        onchange={
                            let name = name.clone();
                            Callback::from(move |value: String| name.set(value))
                        }
                    />
                </Field>

                <Field
                    label="Account"
                    id="service-account"
                    help="Blank means the service's own name."
                >
                    <TextInput
                        id="service-account"
                        value={(*account).clone()}
                        placeholder="svc.adsb"
                        autocomplete="off"
                        onchange={
                            let account = account.clone();
                            Callback::from(move |value: String| account.set(value))
                        }
                    />
                </Field>
            </div>

            <Switch
                id="service-rotate"
                checked={*rotate}
                label="Replace its service token (revokes the one in use)"
                onchange={
                    let rotate = rotate.clone();
                    Callback::from(move |value: bool| rotate.set(value))
                }
            />
        </Card>
    }
}

/// Everything one action produced, shown once.
fn result(onboarding: &ServiceOnboarding, ondismiss: Callback<MouseEvent>) -> Html {
    let expires = format_iso8601(onboarding.enrollment_expires_at);

    html! {
        <section
            class="secret-reveal"
            role="region"
            aria-label={format!("Service {}", onboarding.name)}
        >
            <header class="secret-reveal__header">
                <div>
                    <h3 class="secret-reveal__title">
                        { format!("Service {} — account {}", onboarding.name, onboarding.account) }
                    </h3>
                    <p class="secret-reveal__warning">
                        { "This is the only time these secrets are shown. The server kept a hash \
                           of each and nothing else, so nobody — including an administrator — \
                           can show them again." }
                    </p>
                </div>
                <Button kind={ButtonKind::Subtle} small=true onclick={ondismiss}>{ "Done" }</Button>
            </header>

            <ul class="service-onboarding__notes">
                { for onboarding.notes.iter().map(|note| html! { <li>{ note.clone() }</li> }) }
                <li>{ token_summary(onboarding) }</li>
            </ul>

            <Alert
                kind={AlertKind::Warning}
                title={format!("The enrolment token expires at {expires}.")}
                message="It is one-time: the sidecar's first start spends it on its certificate. \
                         Deploy before then, or add the service again for a fresh one."
            />

            <div class="service-onboarding">
                <Copyable label="Enrolment token (one-time)" value={onboarding.enrollment_token.clone()} />
                if let Some(token) = &onboarding.service_token {
                    <Copyable label="Service token" value={token.clone()} />
                }
                <Copyable label="Configuration: [service]" value={onboarding.config_fragment.clone()} />
                <Copyable label="Environment" value={onboarding.environment.clone()} />
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_account_means_the_services_own_name() {
        let asked = request("  adsb ", "   ", false);

        assert_eq!(asked.name, "adsb");
        assert_eq!(asked.account, None);
        assert!(!asked.rotate_service_token);

        assert_eq!(
            request("adsb", " svc.adsb ", true).account.as_deref(),
            Some("svc.adsb")
        );
    }

    #[test]
    fn a_kept_token_is_described_as_kept_and_a_rotation_as_a_revocation() {
        let mut onboarding: ServiceOnboarding = serde_json::from_value(serde_json::json!({
            "name": "adsb",
            "account": "adsb",
            "account_created": false,
            "enrollment_token": "t",
            "enrollment_token_id": 1,
            "enrollment_expires_at": "2026-09-29T12:00:00Z",
            "service_token_outcome": "kept",
            "config_fragment": "[service]\n",
            "environment": "",
            "notes": [],
        }))
        .unwrap();

        assert!(token_summary(&onboarding).contains("kept"));

        onboarding.service_token_outcome = ServiceTokenOutcome::Rotated;
        assert!(token_summary(&onboarding).contains("revoked"));
    }
}
