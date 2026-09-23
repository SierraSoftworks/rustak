//! The persistent bar across the top of every admin view: the brand, who is
//! signed in, and the button that shows and hides the navigation — a drawer
//! over the page on a narrow screen, a column beside it on a wide one.
//!
//! The navigation itself is not here. There are thirteen destinations and a
//! top bar has room for about five, so the links live in a sidebar beside the
//! page (see [`crate::components::AdminShell`]), which this bar only opens.

use yew::prelude::*;

use crate::app::AuthHandle;
use crate::util::{initials, nav_href};

#[derive(Properties, PartialEq)]
pub struct AppBarProps {
    /// Whether the navigation drawer is open, for the button that toggles it
    /// to say so.
    #[prop_or_default]
    pub menu_open: bool,

    /// Whether that button is closing a drawer laid over the page, and so
    /// draws a cross. Beside the page, the navigation is folded away and
    /// brought back by the same three lines.
    #[prop_or_default]
    pub drawer: bool,

    /// Asked to open or close the navigation drawer.
    #[prop_or_default]
    pub on_menu: Callback<()>,
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

    let on_menu = {
        let on_menu = props.on_menu.clone();
        Callback::from(move |_: MouseEvent| on_menu.emit(()))
    };

    html! {
        <header class="app-bar">
            <div class="app-bar__inner">
                <button
                    type="button"
                    class="app-bar__menu"
                    aria-label={if props.menu_open { "Close the navigation" } else { "Open the navigation" }}
                    aria-controls="admin-nav"
                    aria-expanded={if props.menu_open { "true" } else { "false" }}
                    onclick={on_menu}
                >
                    <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor"
                        stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
                        aria-hidden="true">
                        if props.menu_open && props.drawer {
                            <line x1="6" y1="6" x2="18" y2="18" />
                            <line x1="18" y1="6" x2="6" y2="18" />
                        } else {
                            <line x1="4" y1="7" x2="20" y2="7" />
                            <line x1="4" y1="12" x2="20" y2="12" />
                            <line x1="4" y1="17" x2="20" y2="17" />
                        }
                    </svg>
                </button>
                <a class="app-bar__brand" href={nav_href("/admin")}>
                    <img
                        src="/logo.svg"
                        alt="The rustak logo."
                    />
                    <span class="app-bar__brand-name">{ "rustak" }</span>
                </a>
                { user }
            </div>
        </header>
    }
}
