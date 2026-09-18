//! The sign-in prompt.
//!
//! What it offers comes from `GET /api/v1/auth/metadata`, because only the
//! server knows whether an identity provider is configured. An installation can
//! offer both: single sign-on for everybody, and a passkey so that an
//! administrator still has a way in when the provider is unreachable.

use rustak_api::{AuthMetadata, AuthMode};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{Alert, AlertKind, Button, ButtonKind, LoadingNote};

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
