//! The public page at the site root.
//!
//! It exists for the moment before anybody knows what they are looking at, and
//! it gets out of the way as soon as the session resolves.

use yew::prelude::*;
use yew_router::prelude::*;

use crate::app::{AuthHandle, AuthStatus, Route};
use crate::components::{Button, ButtonKind, Layout};
use crate::fixtures;
use crate::util::{nav_href, window};

#[function_component(Landing)]
pub fn landing() -> Html {
    let auth = use_context::<AuthHandle>();
    let navigator = use_navigator();

    // Once the session resolves to anything other than "sign in first", send the
    // visitor on: somebody already signed in never has to click through this
    // page, and a fresh sign-in lands in the console directly. A refused or
    // errored session is sent through too, so the gate there can explain why.
    // Demo mode navigates full-page, so the `?demo` flag survives.
    {
        let navigator = navigator.clone();
        let status = auth.as_ref().map(|handle| handle.status.clone());
        use_effect_with(status, move |status| {
            let destination = match status {
                None | Some(AuthStatus::Loading) | Some(AuthStatus::NeedsLogin) => None,
                Some(AuthStatus::NeedsSetup) => Some((Route::Setup, "/setup")),
                _ => Some((Route::AdminRoot, "/admin")),
            };

            if let Some((route, path)) = destination {
                if fixtures::is_demo() {
                    let _ = window().location().set_href(&nav_href(path));
                } else if let Some(navigator) = navigator.clone() {
                    navigator.push(&route);
                }
            }
            || ()
        });
    }

    let action = match auth.as_ref() {
        Some(handle) if handle.status == AuthStatus::NeedsLogin => {
            let login = handle.login.clone();
            let onclick = Callback::from(move |_: MouseEvent| login.emit(()));
            html! {
                <Button kind={ButtonKind::Primary} large=true {onclick}>{ "Sign in" }</Button>
            }
        }
        // Either still resolving or about to navigate away: a disabled
        // placeholder, so the call to action does not flicker between states.
        _ => html! {
            <Button
                kind={ButtonKind::Primary}
                large=true
                disabled=true
                onclick={Callback::noop()}
            >
                { "Sign in" }
            </Button>
        },
    };

    html! {
        <Layout>
            <main class="landing">
                <div class="landing__inner">
                    <h1 class="landing__title">{ "rustak" }</h1>
                    <p class="landing__lead">
                        { "A small, self-hosted TAK server. It issues its own certificates, \
                           enrols devices with one-time tokens, routes situational awareness \
                           between them, and keeps the whole thing behind TLS by default." }
                    </p>
                    <div class="landing__actions">{ action }</div>
                </div>
            </main>
        </Layout>
    }
}
