//! The passkeys an account holds: the local half of "how you sign in".
//!
//! Shown on the person's own Credentials page beside the identity-provider
//! link, and on Settings, because registering a second passkey before the first
//! device is lost is the one thing an administrator should do on day one. Both
//! places are this one component, so the ceremony cannot drift between them.

use rustak_api::PasskeySummary;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::auth;
use crate::components::{Alert, AlertKind, Button, ButtonKind, EmptyState, LoadingNote, TextInput};
use crate::util::{format_iso8601, optional_relative};

use crate::pages::load::use_resource;

/// The passkeys the signed-in account holds, with a way to add and remove them.
///
/// Loads its own list, so a page can drop it in without wiring a resource.
#[function_component(PasskeysPanel)]
pub fn passkeys_panel() -> Html {
    let passkeys = use_resource(api::auth::list_passkeys);

    html! {
        <Passkeys
            passkeys={passkeys.data.clone()}
            error={passkeys.error.clone()}
            on_changed={passkeys.reload.clone()}
        />
    }
}

#[derive(Properties, PartialEq)]
struct PasskeysProps {
    passkeys: Option<Vec<PasskeySummary>>,
    error: Option<String>,
    on_changed: Callback<()>,
}

#[function_component(Passkeys)]
fn passkeys(props: &PasskeysProps) -> Html {
    let label = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_register = {
        let (label, busy, error, on_changed) = (
            label.clone(),
            busy.clone(),
            error.clone(),
            props.on_changed.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            let (label, busy, error, on_changed) = (
                label.clone(),
                busy.clone(),
                error.clone(),
                on_changed.clone(),
            );
            let name = if label.trim().is_empty() {
                "This device".to_string()
            } else {
                label.trim().to_string()
            };

            busy.set(true);
            spawn_local(async move {
                match auth::passkey::register(&name, None).await {
                    Ok(_) => {
                        label.set(String::new());
                        error.set(None);
                        on_changed.emit(());
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let list = match (&props.passkeys, &props.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not list your passkeys."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <EmptyState
                title="No passkeys registered"
                message="You are signing in another way. Register one here so that you still \
                    have a way in if that method becomes unavailable."
            />
        },
        (Some(list), _) => html! {
            <ul class="passkey-list">
                { for list.iter().map(|passkey| html! {
                    <PasskeyRow
                        key={passkey.id.get()}
                        passkey={passkey.clone()}
                        on_changed={props.on_changed.clone()}
                        removable={list.len() > 1}
                    />
                }) }
            </ul>
        },
    };

    html! {
        <>
            { list }

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="The passkey could not be registered."
                    message={message.clone()}
                />
            }

            <div class="passkey-add">
                <TextInput
                    id="passkey-label"
                    value={(*label).clone()}
                    placeholder="What to call it, such as “Work laptop”"
                    onchange={Callback::from(move |value| label.set(value))}
                />
                <Button kind={ButtonKind::Primary} busy={*busy} onclick={on_register}>
                    { "Register a passkey" }
                </Button>
            </div>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct PasskeyRowProps {
    passkey: PasskeySummary,
    on_changed: Callback<()>,
    /// Whether removing this one would leave the account with none.
    removable: bool,
}

#[function_component(PasskeyRow)]
fn passkey_row(props: &PasskeyRowProps) -> Html {
    let busy = use_state(|| false);

    let on_remove = {
        let (busy, on_changed, id) = (busy.clone(), props.on_changed.clone(), props.passkey.id);
        Callback::from(move |_: MouseEvent| {
            let (busy, on_changed) = (busy.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                let _ = api::auth::delete_passkey(id.get()).await;
                busy.set(false);
                on_changed.emit(());
            });
        })
    };

    html! {
        <li class="passkey-row">
            <span class="passkey-row__label">{ props.passkey.label.clone() }</span>
            <span class="passkey-row__meta" title={format_iso8601(props.passkey.created_at)}>
                { format!("Last used {}", optional_relative(props.passkey.last_used_at)) }
            </span>
            <Button
                small=true
                kind={ButtonKind::Danger}
                busy={*busy}
                disabled={!props.removable}
                title={(!props.removable)
                    .then_some("This is your only passkey — register another one first.")}
                onclick={on_remove}
            >
                { "Remove" }
            </Button>
        </li>
    }
}
