//! The gate every admin page sits behind.

use yew::prelude::*;

use crate::app::{AuthHandle, AuthStatus};
use crate::components::{Alert, AlertKind, LoadingNote};
use crate::util::nav_href;

use super::Login;

#[derive(Properties, PartialEq)]
pub struct ProtectedProps {
    #[prop_or_default]
    pub children: Html,
}

/// Gates its children behind the resolved authentication state, so they are only
/// mounted — and therefore only fetch — once access has been granted.
#[function_component(Protected)]
pub fn protected(props: &ProtectedProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    match auth.status {
        AuthStatus::Loading => html! { <LoadingNote /> },

        // Nobody can sign in to a server that has never been set up, so the only
        // useful thing to offer is the wizard.
        AuthStatus::NeedsSetup => html! {
            <Alert
                kind={AlertKind::Info}
                title="This server has not been set up yet"
                message="There is no administrator and no certificate authority, so there is \
                    nobody to sign in as. The first-run wizard creates both."
            >
                <a class="btn btn--primary btn--small" href={nav_href("/setup")}>
                    { "Open the setup wizard" }
                </a>
            </Alert>
        },

        AuthStatus::NeedsLogin => html! { <Login /> },

        // Signing in again as the same account cannot change this, so the page
        // must not offer it. Signing *out* is the one thing that helps: it drops
        // this tab's session so somebody can come back as a different account,
        // and the app bar cannot offer it because it only shows a user chip once
        // `/me` has answered.
        AuthStatus::Forbidden => {
            let on_signout = {
                let signout = auth.signout.clone();
                Callback::from(move |_: MouseEvent| signout.emit(()))
            };
            html! {
                <Alert
                    kind={AlertKind::Error}
                    title="Access denied"
                    message="Your account is not permitted to use the admin console. That is \
                        decided by the `admin_acl` expression in the server's `[auth]` \
                        configuration, or by the administrator flag on your account."
                >
                    <button class="btn btn--small btn--primary" onclick={on_signout}>
                        { "Sign out" }
                    </button>
                </Alert>
            }
        }

        AuthStatus::Error(message) => {
            let onclick = Callback::from(|_: MouseEvent| {
                if let Some(window) = web_sys::window() {
                    let _ = window.location().reload();
                }
            });
            html! {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not check your session"
                    message={message}
                >
                    <button class="btn btn--small btn--primary" {onclick}>{ "Reload" }</button>
                </Alert>
            }
        }

        AuthStatus::SignedIn(_) => props.children.clone(),
    }
}
