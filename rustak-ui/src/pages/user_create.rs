//! Adding an account by hand.
//!
//! There is no credential here and no way to add one: rustak has no local
//! passwords, so a new account signs in with a passkey, arrives through the
//! identity provider, or — for a sidecar — is given a credential of its own
//! afterwards. An administrator who could set a secret at creation time would be
//! an administrator who knows it.

use rustak_api::{CreateUserRequest, UserKind, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, Select, SelectOption, TextInput,
};

#[derive(Properties, PartialEq)]
pub struct CreateUserProps {
    pub on_created: Callback<()>,
}

#[function_component(CreateUser)]
pub fn create_user(props: &CreateUserProps) -> Html {
    let username = use_state(String::new);
    let display_name = use_state(String::new);
    let email = use_state(String::new);
    let kind = use_state(UserKind::default);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let parsed = Username::parse(&username);
    // Only complain about what somebody has actually typed: an empty field is
    // not yet a mistake, it is a field they have not reached.
    let invalid = (!username.trim().is_empty())
        .then(|| parsed.as_ref().err().map(|err| err.to_string()))
        .flatten();

    let submit = {
        let (username, display_name, email, kind) = (
            username.clone(),
            display_name.clone(),
            email.clone(),
            kind.clone(),
        );
        let (busy, error, on_created) = (busy.clone(), error.clone(), props.on_created.clone());

        Callback::from(move |_: MouseEvent| {
            let Ok(parsed) = Username::parse(&username) else {
                return;
            };
            let (username, display_name, email) =
                (username.clone(), display_name.clone(), email.clone());
            let (busy, error, on_created) = (busy.clone(), error.clone(), on_created.clone());

            let request = CreateUserRequest {
                username: parsed,
                display_name: some_trimmed(&display_name),
                email: some_trimmed(&email),
                kind: *kind,
            };

            busy.set(true);
            spawn_local(async move {
                match api::users::create(&request).await {
                    Ok(_) => {
                        error.set(None);
                        username.set(String::new());
                        display_name.set(String::new());
                        email.set(String::new());
                        on_created.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let kinds: Vec<SelectOption> = UserKind::ALL
        .iter()
        .map(|kind| SelectOption::new(kind.as_str(), kind.label()))
        .collect();

    html! {
        <Card
            title="Add an account"
            subtitle="No password is set here, because there are none to set."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That account could not be created."
                    message={message.clone()}
                />
            }

            <div class="inline-form">
                <Field
                    label="Username"
                    id="new-username"
                    required=true
                    error={invalid.clone().map(AttrValue::from)}
                    help="What clients authenticate as. It cannot be changed afterwards."
                >
                    <TextInput
                        id="new-username"
                        value={(*username).clone()}
                        placeholder="bhavna"
                        invalid={invalid.is_some()}
                        autocomplete="off"
                        onchange={
                            let username = username.clone();
                            Callback::from(move |value: String| username.set(value))
                        }
                    />
                </Field>

                <Field label="Display name" id="new-display-name">
                    <TextInput
                        id="new-display-name"
                        value={(*display_name).clone()}
                        placeholder="Bhavna Rao"
                        onchange={
                            let display_name = display_name.clone();
                            Callback::from(move |value: String| display_name.set(value))
                        }
                    />
                </Field>

                <Field label="Email" id="new-email">
                    <TextInput
                        id="new-email"
                        value={(*email).clone()}
                        placeholder="bhavna@example.com"
                        onchange={
                            let email = email.clone();
                            Callback::from(move |value: String| email.set(value))
                        }
                    />
                </Field>

                <Field
                    label="Kind"
                    id="new-kind"
                    help="A sidecar never sees a sign-in page; it presents a certificate and a \
                        service token."
                >
                    <Select
                        id="new-kind"
                        value={Some(AttrValue::from(kind.as_str()))}
                        options={kinds}
                        onchange={
                            let kind = kind.clone();
                            Callback::from(move |value: Option<String>| {
                                if let Some(chosen) = value.as_deref().and_then(UserKind::parse) {
                                    kind.set(chosen);
                                }
                            })
                        }
                    />
                </Field>

                <div class="inline-form__action">
                    <Button
                        kind={ButtonKind::Primary}
                        busy={*busy}
                        disabled={parsed.is_err()}
                        title={parsed.is_err().then_some("Give it a valid username first.")}
                        onclick={submit}
                    >
                        { "Create account" }
                    </Button>
                </div>
            </div>
        </Card>
    }
}

/// An empty box means "nothing", not an empty string.
fn some_trimmed(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_string())
}
