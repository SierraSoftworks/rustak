//! One account, in four views.
//!
//! An operator dealing with a person deals with four different things about
//! them — who they are, what they carry, what they can present, and what they
//! can see — and those are four endpoints with four shapes. Tabs rather than one
//! long page, because the reason somebody opened this is usually exactly one of
//! the four and the other three are in the way.

use rustak_api::{User, Username};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::app::Route;
use crate::components::{Alert, AlertKind, LoadingNote};
use crate::util::nav_href;

use super::load::{use_refresh_action, use_resource};
use super::panels::{ChannelsPanel, CredentialsPanel, DevicesPanel, ProfilePanel};

/// The views this page offers, in the order an operator works through them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Profile,
    Devices,
    Credentials,
    Channels,
}

impl Tab {
    const ALL: &'static [Self] = &[
        Self::Profile,
        Self::Devices,
        Self::Credentials,
        Self::Channels,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Devices => "Devices",
            Self::Credentials => "Credentials",
            Self::Channels => "Channels",
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct UserDetailProps {
    /// The account's name, straight out of the route.
    pub username: String,
}

#[function_component(UserDetail)]
pub fn user_detail(props: &UserDetailProps) -> Html {
    // A name that could never have been stored is not an account that is
    // missing — it is one that could not exist, and saying so beats a lookup
    // that fails for a reason nobody can act on.
    let Ok(username) = Username::parse(&props.username) else {
        return html! {
            <Alert
                kind={AlertKind::Error}
                title="That is not a username."
                message={format!("'{}' is not a name this server could have stored.", props.username)}
            />
        };
    };

    html! { <Detail {username} /> }
}

#[derive(Properties, PartialEq)]
struct DetailProps {
    username: Username,
}

#[function_component(Detail)]
fn detail(props: &DetailProps) -> Html {
    let wanted = props.username.clone();
    let user = use_resource(move || {
        let wanted = wanted.clone();
        async move { api::users::get(&wanted).await }
    });
    use_refresh_action(user.reload.clone(), user.busy);

    let tab = use_state(|| Tab::Profile);

    match (&user.data, &user.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not open that account."
                message={message.clone()}
            />
        },
        (Some(found), _) => html! {
            <>
                <Heading user={found.clone()} />
                <Tabs current={*tab} onselect={
                    let tab = tab.clone();
                    Callback::from(move |chosen: Tab| tab.set(chosen))
                } />
                { view(*tab, found, &user.reload) }
            </>
        },
    }
}

/// The body of whichever tab is showing.
fn view(tab: Tab, user: &User, reload: &Callback<()>) -> Html {
    match tab {
        Tab::Profile => html! {
            <ProfilePanel user={user.clone()} on_changed={reload.clone()} />
        },
        Tab::Devices => html! {
            <DevicesPanel username={Some(user.username.clone())} />
        },
        Tab::Credentials => html! {
            <CredentialsPanel username={Some(user.username.clone())} />
        },
        Tab::Channels => html! { <ChannelsPanel username={user.username.clone()} /> },
    }
}

#[derive(Properties, PartialEq)]
struct HeadingProps {
    user: User,
}

/// Who this page is about, and the way back to the list.
#[function_component(Heading)]
fn heading(props: &HeadingProps) -> Html {
    html! {
        <div class="entity-heading">
            <div>
                <h2 class="entity-heading__name">{ props.user.display().to_string() }</h2>
                <p class="entity-heading__meta">
                    <span class="entity-heading__username">
                        { props.user.username.to_string() }
                    </span>
                    { " · " }
                    { props.user.kind.label() }
                </p>
            </div>
            { back_link() }
        </div>
    }
}

/// Demo mode lives in the query string and a client-side navigation replaces the
/// whole URL, so a `Link` out of a demo page would land on one talking to a
/// server that is not there.
fn back_link() -> Html {
    if crate::fixtures::is_demo() {
        return html! {
            <a class="btn btn--small" href={nav_href("/admin/users")}>{ "All accounts" }</a>
        };
    }

    html! {
        <Link<Route> to={Route::Users} classes="btn btn--small">{ "All accounts" }</Link<Route>>
    }
}

#[derive(Properties, PartialEq)]
struct TabsProps {
    current: Tab,
    onselect: Callback<Tab>,
}

#[function_component(Tabs)]
fn tabs(props: &TabsProps) -> Html {
    html! {
        <div class="tabs" role="tablist">
            { for Tab::ALL.iter().map(|tab| {
                let tab = *tab;
                let active = tab == props.current;
                let onclick = {
                    let onselect = props.onselect.clone();
                    Callback::from(move |_: MouseEvent| onselect.emit(tab))
                };

                html! {
                    <button
                        type="button"
                        role="tab"
                        key={tab.label()}
                        class={classes!("tabs__tab", active.then_some("tabs__tab--active"))}
                        aria-selected={active.to_string()}
                        {onclick}
                    >
                        { tab.label() }
                    </button>
                }
            }) }
        </div>
    }
}
