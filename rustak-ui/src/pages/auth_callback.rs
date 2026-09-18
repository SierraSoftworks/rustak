//! Where the identity provider sends the sign-in popup back to.
//!
//! The exchange itself happens in [`crate::app`]'s `use_auth` hook on mount, so
//! that a popup finishes and closes before any of this matters. What is left
//! here is the direct-navigation fallback: somebody whose browser blocked the
//! popup lands on this route in the main window, and once the session resolves
//! they are sent into the console.

use yew::prelude::*;
use yew_router::prelude::*;

use crate::app::{AuthHandle, AuthStatus, Route};
use crate::components::{Alert, AlertKind, Center, Layout};

#[function_component(AuthCallback)]
pub fn auth_callback() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let navigator = use_navigator();

    {
        let navigator = navigator.clone();
        use_effect_with(auth.status.clone(), move |status| {
            let destination = match status {
                AuthStatus::Loading | AuthStatus::Error(_) => None,
                AuthStatus::NeedsSetup => Some(Route::Setup),
                _ => Some(Route::AdminRoot),
            };

            if let (Some(destination), Some(navigator)) = (destination, navigator.clone()) {
                navigator.push(&destination);
            }
            || ()
        });
    }

    let body = match &auth.status {
        AuthStatus::Error(message) => html! {
            <Alert
                kind={AlertKind::Error}
                title="The sign-in could not be completed"
                message={message.clone()}
            />
        },
        _ => html! { <p class="auth-card__lead">{ "Completing sign-in…" }</p> },
    };

    html! {
        <Layout>
            <Center>
                <div class="auth-card">{ body }</div>
            </Center>
        </Layout>
    }
}
