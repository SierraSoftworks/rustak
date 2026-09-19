//! Adding an account by hand.
//!
//! There is no credential here and no way to add one: rustak has no local
//! passwords, so a new account signs in with a passkey, arrives through the
//! identity provider, or — for a sidecar — is given a credential of its own
//! afterwards. An administrator who could set a secret at creation time would be
//! an administrator who knows it.
//!
//! # The CloudTAK shortcut
//!
//! CloudTAK is a program, not a person, and what an operator setting one up
//! actually wants is a service account and then the hand-over on its page —
//! two steps that are always the same two steps. The second button does both,
//! landing on the account's CloudTAK tab; it creates the same row "Create
//! account" would with the kind set to Service, and nothing about the
//! hand-over happens until somebody asks for it there.

use rustak_api::{CreateUserRequest, UserKind, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonGroup, ButtonKind, Card, Field, Select, SelectOption, TextInput,
};
use crate::util::{nav_href, urlencode, window};

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

    let fields = Fields {
        username: username.clone(),
        display_name: display_name.clone(),
        email: email.clone(),
    };

    let submit = {
        let (fields, kind) = (fields.clone(), kind.clone());
        let (busy, error, on_created) = (busy.clone(), error.clone(), props.on_created.clone());

        Callback::from(move |_: MouseEvent| {
            let on_created = on_created.clone();

            create(
                &fields,
                *kind,
                &busy,
                &error,
                Callback::from(move |_: Username| on_created.emit(())),
            );
        })
    };

    let onboard = {
        let (fields, busy, error) = (fields.clone(), busy.clone(), error.clone());

        Callback::from(move |_: MouseEvent| {
            create(
                &fields,
                // CloudTAK never sees a sign-in page; it presents a certificate
                // and a password, which is what a service account is for.
                UserKind::Service,
                &busy,
                &error,
                Callback::from(open_cloudtak_tab),
            );
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
                    <ButtonGroup label="Create">
                        <Button
                            kind={ButtonKind::Primary}
                            busy={*busy}
                            disabled={parsed.is_err()}
                            title={parsed.is_err().then_some("Give it a valid username first.")}
                            onclick={submit}
                        >
                            { "Create account" }
                        </Button>
                        <Button
                            busy={*busy}
                            disabled={parsed.is_err()}
                            title={parsed.is_err().then_some("Give it a valid username first.")}
                            onclick={onboard}
                        >
                            { "Create CloudTAK account" }
                        </Button>
                    </ButtonGroup>
                </div>
            </div>
        </Card>
    }
}

/// The three text boxes, so that both buttons read the same ones and clear the
/// same ones.
#[derive(Clone)]
struct Fields {
    username: UseStateHandle<String>,
    display_name: UseStateHandle<String>,
    email: UseStateHandle<String>,
}

/// Creates the account and hands its name to `then`.
///
/// The boxes are cleared only on success, so a refused name is still there to
/// correct rather than gone with the error that explained it.
fn create(
    fields: &Fields,
    kind: UserKind,
    busy: &UseStateHandle<bool>,
    error: &UseStateHandle<Option<String>>,
    then: Callback<Username>,
) {
    let Ok(parsed) = Username::parse(&fields.username) else {
        return;
    };

    let request = CreateUserRequest {
        username: parsed,
        display_name: some_trimmed(&fields.display_name),
        email: some_trimmed(&fields.email),
        kind,
    };

    let (fields, busy, error) = (fields.clone(), busy.clone(), error.clone());

    busy.set(true);
    spawn_local(async move {
        match api::users::create(&request).await {
            Ok(created) => {
                error.set(None);
                fields.username.set(String::new());
                fields.display_name.set(String::new());
                fields.email.set(String::new());
                then.emit(created.username);
            }
            Err(err) => error.set(Some(err.to_string())),
        }
        busy.set(false);
    });
}

/// Opens the new account's CloudTAK tab.
///
/// A whole-page navigation rather than a router push, for the same reason the
/// detail page's back link is one: demo mode lives in the query string, and a
/// client-side navigation replaces the URL with one that has lost it.
fn open_cloudtak_tab(username: Username) {
    let path = nav_href(&format!("/admin/users/{}", urlencode(username.as_str())));

    // The fragment goes after the query, which is why it is appended here
    // rather than passed through `nav_href`.
    let _ = window().location().set_href(&format!("{path}#cloudtak"));
}

/// An empty box means "nothing", not an empty string.
fn some_trimmed(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_string())
}
