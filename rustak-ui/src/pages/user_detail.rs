//! One account, in six views.
//!
//! An operator dealing with a person deals with several different things about
//! them — who they are, what they carry, what they can present, what they can
//! see, and how to get a client configured when it cannot enrol — and those
//! are six endpoints with six shapes. Tabs rather than one long page,
//! because the reason somebody opened this is usually exactly one of them and
//! the rest are in the way.
//!
//! # Why one tab is addressable and the others are not
//!
//! The Users page offers "Create CloudTAK account", which creates the account
//! and then has somewhere to send the operator. A fragment (`#cloudtak`) is
//! what makes that possible without turning every tab into a route: it is read
//! once, when the page mounts, and never written — so a reload lands where the
//! link did and clicking between tabs afterwards behaves as it always has.

use rustak_api::{User, Username};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, LoadingNote, PageHeading, TabBar};

use super::load::{use_refresh_action, use_resource};
use super::panels::{
    ChannelsPanel, CloudTakPanel, ConfigPackagePanel, CredentialsPanel, DevicesPanel, ProfilePanel,
};

/// The views this page offers, in the order an operator works through them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Profile,
    Devices,
    Credentials,
    Channels,
    Package,
    CloudTak,
}

impl Tab {
    const ALL: &'static [Self] = &[
        Self::Profile,
        Self::Devices,
        Self::Credentials,
        Self::Channels,
        Self::Package,
        Self::CloudTak,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Devices => "Devices",
            Self::Credentials => "Credentials",
            Self::Channels => "Channels",
            Self::Package => "Package",
            Self::CloudTak => "CloudTAK",
        }
    }

    /// The tab a `#fragment` names, where it names one.
    fn from_fragment(fragment: &str) -> Option<Self> {
        let wanted = fragment.trim_start_matches('#').to_ascii_lowercase();

        Self::ALL
            .iter()
            .copied()
            .find(|tab| tab.label().to_ascii_lowercase() == wanted)
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

    // Read once, on mount: the fragment says where to open, not where the
    // operator is now, and rewriting it on every click would make the back
    // button walk through tabs.
    let tab = use_state(opening_tab);

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
                { heading(found) }
                { tabs(
                    *tab,
                    {
                        let tab = tab.clone();
                        Callback::from(move |chosen: Tab| tab.set(chosen))
                    },
                    view(*tab, found, &user.reload),
                ) }
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
        Tab::Package => html! { <ConfigPackagePanel username={user.username.clone()} /> },
        Tab::CloudTak => html! { <CloudTakPanel username={user.username.clone()} /> },
    }
}

/// The tab this page opens on, from the URL fragment where there is one.
fn opening_tab() -> Tab {
    crate::util::window()
        .location()
        .hash()
        .ok()
        .as_deref()
        .and_then(Tab::from_fragment)
        .unwrap_or(Tab::Profile)
}

/// Whose page this is, in the shell's title row, with the way back to the list.
fn heading(user: &User) -> Html {
    let subtitle = html! {
        <>
            <code>{ user.username.to_string() }</code>
            { " · " }
            { user.kind.label() }
        </>
    };

    html! {
        <PageHeading
            title={user.display().to_string()}
            {subtitle}
            parent="Users"
            parent_path="/admin/users"
        />
    }
}

/// The tab strip, with the chosen tab's view under it.
fn tabs(current: Tab, onselect: Callback<Tab>, view: Html) -> Html {
    let onselect = onselect.reform(|label: &'static str| {
        Tab::ALL
            .iter()
            .copied()
            .find(|tab| tab.label() == label)
            .unwrap_or(Tab::ALL[0])
    });

    html! {
        <TabBar
            tabs={Tab::ALL.iter().map(|tab| tab.label()).collect::<Vec<_>>()}
            current={current.label()}
            {onselect}
        >
            { view }
        </TabBar>
    }
}
