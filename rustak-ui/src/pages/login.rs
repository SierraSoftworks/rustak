//! The sign-in prompt.
//!
//! What it offers comes from `GET /api/v1/auth/metadata`, because only the
//! server knows whether an identity provider is configured. An installation can
//! offer both: single sign-on for everybody, and a passkey so that an
//! administrator still has a way in when the provider is unreachable.
//!
//! # Why the passkey button asks for nothing
//!
//! The primary passkey flow names nobody: the authenticator says which account
//! it holds. Asking for a username first would make this page a way to find out
//! which accounts exist, and every passkey this server registers is
//! discoverable precisely so that it does not have to
//! (`rustak-server/src/auth/passkeys.rs`).
//!
//! The fallback below exists for the keys that are not: one registered against
//! another server and moved here, one from an authenticator that ignored the
//! request to store it, or one registered by a version of rustak that asked for
//! `residentKey: "discouraged"`. Naming an account there is a choice somebody
//! makes after the username-less prompt has already found nothing, and the
//! server answers a username with no passkey exactly as it answers one that
//! does not exist.

use rustak_api::{AuthMetadata, AuthMode, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::auth;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Field, LoadingNote, TextInput};

#[function_component(Login)]
pub fn login() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let metadata = use_state(|| None::<AuthMetadata>);
    let error = use_state(|| None::<String>);

    {
        let metadata = metadata.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match api::auth::metadata().await {
                    Ok(loaded) => metadata.set(Some(loaded)),
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
            || ()
        });
    }

    let on_sso = {
        let login = auth.login.clone();
        Callback::from(move |_: MouseEvent| login.emit(()))
    };
    let on_passkey = {
        let login_passkey = auth.login_passkey.clone();
        Callback::from(move |_: MouseEvent| login_passkey.emit(()))
    };

    let body = match (&*metadata, &*error) {
        (_, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not reach this server"
                message={message.clone()}
            />
        },
        (None, None) => html! { <LoadingNote label="Checking how to sign in…" /> },
        (Some(metadata), None) => {
            let sso = matches!(metadata.mode, AuthMode::Oidc { .. });
            let passkeys = metadata.passkeys_enabled;

            if !sso && !passkeys {
                html! {
                    <Alert
                        kind={AlertKind::Warning}
                        title="No way to sign in is configured"
                        message="This server has neither an identity provider nor passkeys \
                            enabled. Configure `[auth.oidc]`, or register a passkey with the \
                            rustak command line, and then reload this page."
                    />
                }
            } else {
                html! {
                    <>
                        if let Some(message) = &auth.login_error {
                            <Alert
                                kind={AlertKind::Error}
                                title="The sign-in could not be completed"
                                message={message.clone()}
                            />
                        }
                        <div class="auth-card__actions">
                            if sso {
                                <Button kind={ButtonKind::Primary} large=true onclick={on_sso}>
                                    { "Sign in with single sign-on" }
                                </Button>
                            }
                            if passkeys {
                                <Button
                                    kind={if sso { ButtonKind::Default } else { ButtonKind::Primary }}
                                    large=true
                                    onclick={on_passkey}
                                >
                                    { "Sign in with a passkey" }
                                </Button>
                            }
                        </div>
                        if passkeys {
                            <NamedPasskey on_signed_in={auth.refresh.clone()} />
                        }
                    </>
                }
            }
        }
    };

    html! {
        <div class="auth-screen">
            <div class="auth-card">
                <h1 class="auth-card__title">{ "Sign in" }</h1>
                <p class="auth-card__lead">
                    { "The rustak console manages this server's identities, channels and \
                       certificates. You need an account here to use it." }
                </p>
                { body }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct NamedPasskeyProps {
    /// Re-resolves the session once the ceremony has established one, the same
    /// way the discoverable button does.
    pub on_signed_in: Callback<()>,
}

/// The username-assisted passkey sign-in, hidden until somebody asks for it.
///
/// Folded away by default so that the prompt still has one obvious thing to do,
/// and so that a username field is never the first thing offered.
#[function_component(NamedPasskey)]
pub fn named_passkey(props: &NamedPasskeyProps) -> Html {
    let open = use_state(|| false);
    let username = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // The rules live in `rustak-api`, so the page asks rather than deciding.
    let parsed = Username::parse(&username).ok();

    let on_toggle = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };

    let on_submit = {
        let (username, busy, error) = (username.clone(), busy.clone(), error.clone());
        let on_signed_in = props.on_signed_in.clone();

        Callback::from(move |_: MouseEvent| {
            let Ok(named) = Username::parse(&username) else {
                return;
            };

            let (busy, error, on_signed_in) = (busy.clone(), error.clone(), on_signed_in.clone());
            busy.set(true);
            spawn_local(async move {
                match auth::passkey::login(Some(named)).await {
                    Ok(()) => {
                        error.set(None);
                        on_signed_in.emit(());
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    if !*open {
        return html! {
            <p class="auth-card__aside">
                <button type="button" class="link-button" onclick={on_toggle}>
                    { "Sign in with a username instead" }
                </button>
            </p>
        };
    }

    html! {
        <div class="auth-card__aside">
            <Field
                id="login-username"
                label="Username"
                help="Only needed for a passkey your browser cannot offer on its own — one \
                    registered elsewhere, or by an older version of this server."
            >
                <TextInput
                    id="login-username"
                    value={(*username).clone()}
                    placeholder="avery"
                    onchange={let username = username.clone(); Callback::from(move |v| username.set(v))}
                />
            </Field>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That passkey could not sign you in."
                    message={message.clone()}
                />
            }

            <Button
                kind={ButtonKind::Default}
                busy={*busy}
                disabled={parsed.is_none()}
                onclick={on_submit}
            >
                { "Sign in as this account" }
            </Button>
        </div>
    }
}
