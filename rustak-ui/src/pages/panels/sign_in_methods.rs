//! How the signed-in account is signed in to, and how to add a way.
//!
//! An installation moving from passkeys to single sign-on has a problem the
//! server cannot solve on its own: the person signing in through the provider
//! for the first time already has an account here, under the same name, with
//! devices and channels on it. Creating a second account is wrong, and handing
//! the first to whoever the directory says owns the name is a decision only an
//! operator may make (`link_by_username`). What a *person* may do is prove
//! they hold both — they are signed in here, and they can sign in there — and
//! that is what the button below does.

use rustak_api::{AuthMode, Me, UserSource};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::auth;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Card, LoadingNote};
use crate::pages::load::use_resource;

#[derive(Properties, PartialEq)]
pub struct SignInMethodsProps {
    /// The account, as `/me` last described it.
    pub user: Me,
    /// Re-resolves the session once the account has changed.
    pub on_changed: Callback<()>,
}

/// The identity-provider half of "how you sign in", as a card of its own: the
/// link to the provider is the card's one action, and it lives in the footer.
#[function_component(SignInMethods)]
pub fn sign_in_methods(props: &SignInMethodsProps) -> Html {
    let metadata = use_resource(api::auth::metadata);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let linked_now = use_state(|| false);

    let on_link = {
        let (busy, error, linked_now) = (busy.clone(), error.clone(), linked_now.clone());
        let on_changed = props.on_changed.clone();
        Callback::from(move |_: MouseEvent| {
            let (busy, error, linked_now) = (busy.clone(), error.clone(), linked_now.clone());
            let on_changed = on_changed.clone();
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match auth::oidc::begin_link().await {
                    Ok(Some(_)) => {
                        linked_now.set(true);
                        on_changed.emit(());
                    }
                    // Closed without finishing: nothing to say.
                    Ok(None) => {}
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let card = |body: Html, footer: Option<Html>| {
        html! {
            <Card
                title="How you sign in"
                subtitle="Single sign-on, passkeys, or both. Keep a second way in."
                {footer}
            >
                <div class="sign-in-methods">{ body }</div>
            </Card>
        }
    };

    let provider = match (&metadata.data, &metadata.error) {
        (None, None) => {
            return card(
                html! { <LoadingNote label="Checking how you can sign in…" /> },
                None,
            );
        }
        (Some(metadata), _) => match &metadata.mode {
            AuthMode::Oidc { .. } => Some(metadata),
            _ => None,
        },
        (None, Some(_)) => None,
    };

    let body = match (&props.user.source, provider) {
        (UserSource::Oidc, _) => {
            let issuer = props
                .user
                .identity_provider
                .clone()
                .unwrap_or_else(|| "your identity provider".to_string());
            html! {
                <Alert
                    kind={if *linked_now { AlertKind::Success } else { AlertKind::Info }}
                    title={if *linked_now { "Your account is now linked" } else { "Linked to single sign-on" }}
                    message={format!(
                        "This account signs in through {issuer}. Your name, email and channels \
                         follow what it says about you each time you sign in. A passkey below \
                         still works, and is worth keeping for when the provider is unreachable."
                    )}
                />
            }
        }
        (UserSource::Service, _) => html! {
            <p class="muted">{ "A service account signs in with its credential, not a person's identity." }</p>
        },
        (UserSource::Local, None) => html! {
            <p class="muted">
                { "This account was made here and signs in with a passkey. This server has no \
                   identity provider configured to link it to." }
            </p>
        },
        (UserSource::Local, Some(_)) => {
            let footer = html! {
                <Button kind={ButtonKind::Primary} busy={*busy} onclick={on_link}>
                    { "Link your single sign-on account" }
                </Button>
            };

            return card(
                html! {
                    <>
                        <p>
                            { "This account was made here and signs in with a passkey. Signing \
                               in through single sign-on would create a second account under \
                               your name — link this one instead. You keep your devices, \
                               credentials, channels and passkeys; your username will follow \
                               what the provider calls you." }
                        </p>
                        if let Some(message) = &*error {
                            <Alert
                                kind={AlertKind::Error}
                                title="The account could not be linked"
                                message={message.clone()}
                            />
                        }
                    </>
                },
                Some(footer),
            );
        }
    };

    card(body, None)
}
