//! The persistent bar across the top of every admin view: the brand, and who is
//! signed in.
//!
//! The navigation is not here. There are eleven destinations and a top bar has
//! room for about five, so the links live in their own strip underneath (see
//! [`crate::components::AdminShell`]) where they can wrap without pushing the
//! user chip off the end.

use yew::prelude::*;

use crate::app::AuthHandle;
use crate::util::{initials, nav_href};

#[derive(Properties, PartialEq)]
pub struct AppBarProps {
    /// The navigation strip, rendered below the bar's own row.
    #[prop_or_default]
    pub children: Html,
}

#[function_component(AppBar)]
pub fn app_bar(props: &AppBarProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let user = match &auth.user {
        Some(user) => {
            let on_signout = {
                let signout = auth.signout.clone();
                Callback::from(move |_: MouseEvent| signout.emit(()))
            };

            let email = match &user.email {
                Some(email) => html! { <span class="user-chip__email">{ email.clone() }</span> },
                None => html! {
                    <span class="user-chip__email">{ user.username.to_string() }</span>
                },
            };

            html! {
                <div class="user-chip">
                    <span class="user-chip__avatar">{ initials(user.display()) }</span>
                    <span class="user-chip__meta">
                        <span class="user-chip__name">{ user.display().to_string() }</span>
                        { email }
                    </span>
                    <button class="user-chip__signout" onclick={on_signout}>{ "Sign out" }</button>
                </div>
            }
        }
        None => html! {},
    };

    html! {
        <header class="app-bar">
            <div class="app-bar__inner">
                <a class="app-bar__brand" href={nav_href("/admin")}>
                    <img
                        src="https://cdn.sierrasoftworks.com/logos/icon.svg"
                        alt="The Sierra Softworks logo."
                    />
                    <span class="app-bar__brand-name">{ "rustak" }</span>
                </a>
                { user }
            </div>
            { props.children.clone() }
        </header>
    }
}
