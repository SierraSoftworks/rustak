//! The first three steps: prove possession, create the administrator, and give
//! them a way to sign in.

use rustak_api::{CreateAdminRequest, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::auth;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Field, SecretInput, TextInput};
use crate::fixtures;

#[derive(Properties, PartialEq)]
pub struct TokenStepProps {
    pub token: String,
    pub on_token: Callback<String>,
    pub on_next: Callback<()>,
}

/// Step one: the one-time token the server wrote to `[auth].setup_token_file`
/// and logged at first start.
///
/// It is a proof of possession rather than a secret in its own right — whoever
/// can read that file already has the server's filesystem — but it is what stops
/// the wizard being reachable by whoever finds the port first.
#[function_component(TokenStep)]
pub fn token_step(props: &TokenStepProps) -> Html {
    let ready = !props.token.trim().is_empty();
    let on_next = {
        let on_next = props.on_next.clone();
        Callback::from(move |_: MouseEvent| on_next.emit(()))
    };

    html! {
        <>
            <p class="wizard__lead">
                { "This server wrote a one-time setup token to the file named by " }
                <code>{ "[auth].setup_token_file" }</code>
                { " and logged it at first start. Paste it here." }
            </p>

            if fixtures::is_demo() {
                <Alert
                    kind={AlertKind::Info}
                    title="Demo mode"
                    message="There is no server behind this page, so any token of eight \
                        characters or more is accepted."
                />
            }

            <Field
                id="setup-token"
                label="Setup token"
                required=true
                help="It is spent the moment the first administrator is created."
            >
                <SecretInput
                    id="setup-token"
                    value={props.token.clone()}
                    onchange={props.on_token.clone()}
                    placeholder="Paste the token"
                />
            </Field>

            <Button kind={ButtonKind::Primary} disabled={!ready} onclick={on_next}>
                { "Continue" }
            </Button>
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct AdminStepProps {
    pub token: String,

    /// Hands back the username and the short-lived registration token the next
    /// step needs.
    pub on_created: Callback<(Username, String)>,

    /// Returns to the token step. The token and this form feed one request, so a
    /// token the server refuses is discovered here — and it would be a poor
    /// wizard that made somebody reload the page to correct it.
    pub on_back: Callback<()>,
}

/// Step two: the first administrator.
///
/// No password is asked for, because rustak has none. The response carries a
/// registration token good for one thing — registering this person's first
/// passkey — which is what the next step spends.
#[function_component(AdminStep)]
pub fn admin_step(props: &AdminStepProps) -> Html {
    let username = use_state(String::new);
    let display_name = use_state(String::new);
    let email = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // The rules live in `rustak-api`, so the page asks rather than deciding.
    let parsed = Username::parse(&username);
    let invalid = (!username.is_empty()).then(|| parsed.as_ref().err().map(|err| err.to_string()));

    let on_submit = {
        let (username, display_name, email) =
            (username.clone(), display_name.clone(), email.clone());
        let (busy, error) = (busy.clone(), error.clone());
        let (token, on_created) = (props.token.clone(), props.on_created.clone());

        Callback::from(move |_: MouseEvent| {
            let Ok(parsed) = Username::parse(&username) else {
                return;
            };
            let request = CreateAdminRequest {
                setup_token: token.clone(),
                username: parsed.clone(),
                display_name: optional(&display_name),
                email: optional(&email),
            };

            let (busy, error, on_created) = (busy.clone(), error.clone(), on_created.clone());
            busy.set(true);
            spawn_local(async move {
                match api::setup::create_admin(&request).await {
                    Ok(created) => {
                        error.set(None);
                        on_created.emit((parsed, created.registration_token));
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
                { "This account will be able to enrol devices, issue certificates and manage \
                   everybody else." }
            </p>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="The administrator could not be created."
                    message={message.clone()}
                />
            }

            <Field
                id="admin-username"
                label="Username"
                required=true
                help="Lower case, and the name a client certificate will be issued in."
                error={invalid.flatten()}
            >
                <TextInput
                    id="admin-username"
                    value={(*username).clone()}
                    autocomplete="username webauthn"
                    placeholder="avery"
                    onchange={let username = username.clone(); Callback::from(move |v| username.set(v))}
                />
            </Field>

            <Field id="admin-display" label="Display name">
                <TextInput
                    id="admin-display"
                    value={(*display_name).clone()}
                    placeholder="Avery Quinn"
                    onchange={let display_name = display_name.clone(); Callback::from(move |v| display_name.set(v))}
                />
            </Field>

            <Field id="admin-email" label="Email">
                <TextInput
                    id="admin-email"
                    value={(*email).clone()}
                    placeholder="avery@example.com"
                    onchange={let email = email.clone(); Callback::from(move |v| email.set(v))}
                />
            </Field>

            <div class="wizard__actions">
                <Button
                    kind={ButtonKind::Primary}
                    busy={*busy}
                    disabled={parsed.is_err()}
                    onclick={on_submit}
                >
                    { "Create the administrator" }
                </Button>
                <Button
                    kind={ButtonKind::Subtle}
                    onclick={let on_back = props.on_back.clone();
                        Callback::from(move |_: MouseEvent| on_back.emit(()))}
                >
                    { "Change the setup token" }
                </Button>
            </div>
        </>
    }
}

fn optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Properties, PartialEq)]
pub struct PasskeyStepProps {
    pub username: Username,
    pub registration_token: String,
    pub on_registered: Callback<()>,
}

/// Step three: the passkey that administrator will sign in with.
///
/// It is the only way in, so the wizard does not offer to skip it. If the
/// server did not hand back a session with the new passkey, a sign-in ceremony
/// follows immediately — the wizard's remaining steps need one.
#[function_component(PasskeyStep)]
pub fn passkey_step(props: &PasskeyStepProps) -> Html {
    let label = use_state(|| "This device".to_string());
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_register = {
        let (label, busy, error) = (label.clone(), busy.clone(), error.clone());
        let (username, token, on_registered) = (
            props.username.clone(),
            props.registration_token.clone(),
            props.on_registered.clone(),
        );

        Callback::from(move |_: MouseEvent| {
            let (label, busy, error) = ((*label).clone(), busy.clone(), error.clone());
            let (username, token, on_registered) =
                (username.clone(), token.clone(), on_registered.clone());

            busy.set(true);
            spawn_local(async move {
                let outcome = match auth::passkey::register(&label, Some(token)).await {
                    // A server that established a session with the registration
                    // saved us a second prompt.
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => auth::passkey::login(Some(username)).await,
                    Err(message) => Err(message),
                };

                match outcome {
                    Ok(()) => {
                        error.set(None);
                        on_registered.emit(());
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    html! {
        <>
            <p class="wizard__lead">
                { format!(
                    "There are no passwords in rustak. Register a passkey for {} now — it is \
                     how you will sign in from here on.",
                    props.username,
                ) }
            </p>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="The passkey could not be registered."
                    message={message.clone()}
                >
                    <p class="alert__message">
                        { "You can try again; the registration token is good for a few \
                           minutes." }
                    </p>
                </Alert>
            }

            <Field
                id="passkey-label"
                label="What to call this passkey"
                help="So you can tell it apart from the next one you register."
            >
                <TextInput
                    id="passkey-label"
                    value={(*label).clone()}
                    onchange={let label = label.clone(); Callback::from(move |v| label.set(v))}
                />
            </Field>

            <Button kind={ButtonKind::Primary} busy={*busy} onclick={on_register}>
                { "Register a passkey" }
            </Button>
        </>
    }
}
