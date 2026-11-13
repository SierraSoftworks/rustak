//! The chrome shared by every admin view, which is yewi's `AppShell`: an app
//! bar, a sidebar that becomes a drawer on a narrow screen, and the page.
//!
//! It draws the page's title row with the slots a page can fill (see
//! [`super::chrome`]), carries the page's search box in the app bar so it
//! stays put while the page scrolls, and gates the routed page behind the
//! session — so a page only mounts, and therefore only fetches, once access
//! has been granted.

use chrono::{Datelike, Utc};
use yew::prelude::*;
use yew_router::prelude::*;
use yewi::{
    AppBar, AppBarBrand, AppShell, Breadcrumbs, Crumb, IconData, NavTarget, PageHeader,
    SearchInput, Sidebar, SidebarGroup, SidebarItem, ThemeToggle, UserMenu, icons as yi,
};

use super::chrome::{Heading, PageActions, PageSearch, search_of};
use crate::app::{AuthHandle, Route};
use crate::fixtures;
use crate::pages::Protected;
use crate::util::nav_href;

#[derive(Properties, PartialEq)]
pub struct AdminShellProps {
    #[prop_or_default]
    pub children: Html,
}

/// Where a wide screen remembers that its sidebar was hidden: somebody who
/// wants the width for a map wants it next time too.
const NAV_COLLAPSED: &str = "rustak.admin.nav_folded";

/// A link to an in-app path. Demo mode lives in the query string and a
/// client-side navigation replaces the whole URL, so in demo mode a link is a
/// full navigation that carries `?demo` with it.
fn target(route: &Route) -> NavTarget {
    if fixtures::is_demo() {
        NavTarget::href(nav_href(&route.to_path()))
    } else {
        NavTarget::route(route.clone())
    }
}

#[function_component(AdminShell)]
pub fn admin_shell(props: &AdminShellProps) -> Html {
    let actions = use_state(Html::default);
    let controls = use_state(Html::default);
    let heading = use_state(|| None::<Heading>);
    let query = use_state(AttrValue::default);

    // Memoised so the context identity stays stable and pages do not
    // re-render each other when one of them fills a slot.
    let chrome = {
        let (actions, controls, heading) = (actions.clone(), controls.clone(), heading.clone());
        use_memo((), move |_| PageActions {
            actions: Callback::from(move |content: Html| actions.set(content)),
            controls: Callback::from(move |content: Html| controls.set(content)),
            heading: Callback::from(move |value: Option<Heading>| heading.set(value)),
        })
    };

    // The shell is shared, so what it is a shell *for* comes from the route.
    let route = use_route::<Route>().unwrap_or(Route::Dashboard);
    let (title, subtitle) = route.heading();

    // A map is the one page that is better for every pixel: it takes the
    // whole main area, and what the title row would have said is laid over it.
    let immersive = route == Route::Map;

    // What a page put in the shell belongs to that page. Clearing it on every
    // route change stops one page's search narrowing the next one's list.
    {
        let (actions, controls) = (actions.clone(), controls.clone());
        let (heading, query) = (heading.clone(), query.clone());
        use_effect_with(route.clone(), move |_| {
            actions.set(Html::default());
            controls.set(Html::default());
            heading.set(None);
            query.set(AttrValue::default());
            || ()
        });
    }

    let search = search_of(&route);
    let bar = html! {
        <AppBar
            brand={html! {
                <AppBarBrand
                    logo={html! { <img src="/logo.svg" alt="" /> }}
                    href={target(&Route::Dashboard)}
                    label="rustak dashboard"
                >
                    { "rustak" }
                </AppBarBrand>
            }}
            actions={html! { <><ThemeToggle /><UserArea /></> }}
            menu_label="Navigation"
            wrap={search.is_some()}
        >
            if let Some((label, placeholder)) = search {
                <SearchInput
                    class="shell__search"
                    value={(*query).clone()}
                    {label}
                    {placeholder}
                    on_input={
                        let query = query.clone();
                        Callback::from(move |value: String| query.set(value.into()))
                    }
                />
            }
        </AppBar>
    };

    let actions = html! { <>{ (*controls).clone() }{ (*actions).clone() }</> };
    let header = if immersive {
        // The page is still headed, for whoever is reading it rather than
        // looking at it.
        html! { <h1 class="visually-hidden">{ title }</h1> }
    } else {
        match &*heading {
            Some(heading) => html! {
                <PageHeader
                    class="page-head"
                    title={heading.title.clone()}
                    subtitle={heading.subtitle.clone()}
                    meta={heading.meta.clone()}
                    breadcrumbs={crumbs(heading)}
                    actions={actions.clone()}
                />
            },
            None => html! {
                <PageHeader
                    class="page-head"
                    {title}
                    subtitle={html! { subtitle }}
                    actions={actions.clone()}
                />
            },
        }
    };

    html! {
        <AppShell
            class={classes!("shell", immersive.then_some("shell--immersive"))}
            {bar}
            sidebar={html! { <AdminNav current={route.clone()} /> }}
            storage_key={NAV_COLLAPSED}
            fill={immersive}
            flush={immersive}
        >
            <ContextProvider<PageActions> context={(*chrome).clone()}>
                <ContextProvider<PageSearch> context={PageSearch((*query).clone())}>
                    <Protected>
                        { header }
                        { props.children.clone() }
                    </Protected>
                </ContextProvider<PageSearch>>
            </ContextProvider<PageActions>>
            if !immersive {
                <footer class="app-footer">
                    <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
                </footer>
            }
        </AppShell>
    }
}

/// The way back from one thing to the list it is a row of.
fn crumbs(heading: &Heading) -> Html {
    let Some((label, path)) = &heading.parent else {
        return Html::default();
    };

    html! {
        <Breadcrumbs>
            <Crumb href={NavTarget::href(nav_href(path))}>{ label.clone() }</Crumb>
            <Crumb current=true>{ heading.title.clone() }</Crumb>
        </Breadcrumbs>
    }
}

/// Who is signed in, their own page, and the way out.
#[function_component(UserArea)]
fn user_area() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let Some(user) = &auth.user else {
        return Html::default();
    };

    let detail = user
        .email
        .clone()
        .unwrap_or_else(|| user.username.to_string());

    html! {
        <UserMenu
            name={user.display().to_string()}
            {detail}
            show_name=true
            on_sign_out={auth.signout.clone()}
        >
            <yewi::MenuItem href={target(&Route::Account)} icon={yi::USER_ROUND}>
                { "Your account" }
            </yewi::MenuItem>
        </UserMenu>
    }
}

/// One destination: its label, its icon and its route.
type NavLink = (&'static str, IconData, Route);

/// Every destination, grouped by what somebody is there to do: what is
/// happening, who and what connects, what they exchange, and how the server
/// itself is set up.
const NAV: &[(&str, &[NavLink])] = &[
    (
        "Overview",
        &[
            ("Dashboard", yi::LAYOUT_DASHBOARD, Route::Dashboard),
            ("Map", yi::MAP, Route::Map),
            ("Situation", yi::RADAR, Route::Situation),
            ("Activity", yi::SCROLL_TEXT, Route::Activity),
        ],
    ),
    (
        "Clients",
        &[
            ("Users", yi::USERS, Route::Users),
            ("EUDs", yi::SMARTPHONE, Route::Euds),
            ("Profiles", yi::SLIDERS_HORIZONTAL, Route::Profiles),
        ],
    ),
    (
        "Operations",
        &[
            ("Channels", yi::RADIO, Route::Channels),
            ("Missions", yi::FLAG, Route::Missions),
            ("Packages", yi::PACKAGE, Route::Packages),
        ],
    ),
    (
        "Settings",
        &[
            ("Account", yi::USER_ROUND, Route::Account),
            ("Security", yi::SHIELD_CHECK, Route::Security),
            ("Storage", yi::HARD_DRIVE, Route::Storage),
            ("Services", yi::PLUG, Route::Services),
        ],
    ),
];

/// The destination a route is somewhere inside: one account's page is a row
/// of Users, so the list must not read as though nothing is selected while it
/// is open. The same for a mission and a profile.
fn section(route: &Route) -> Route {
    match route {
        Route::AdminRoot => Route::Dashboard,
        Route::UserDetail { .. } => Route::Users,
        Route::MissionDetail { .. } => Route::Missions,
        Route::ProfileEditor { .. } => Route::Profiles,
        other => other.clone(),
    }
}

#[derive(Properties, PartialEq)]
struct AdminNavProps {
    current: Route,
}

/// The links between the admin pages, down the side of every one of them.
#[function_component(AdminNav)]
fn admin_nav(props: &AdminNavProps) -> Html {
    let current = section(&props.current);

    html! {
        <Sidebar label="Admin sections">
            { for NAV.iter().map(|(group, links)| html! {
                <SidebarGroup label={*group}>
                    { for links.iter().map(|(label, icon, route)| html! {
                        <SidebarItem href={target(route)} icon={*icon} current={*route == current}>
                            { *label }
                        </SidebarItem>
                    }) }
                </SidebarGroup>
            }) }
            // Only reachable in demo mode, which is the only mode it works in.
            if fixtures::is_demo() {
                <SidebarGroup label="Development">
                    <SidebarItem href={NavTarget::href(nav_href("/demo/controls"))} icon={yi::COMPONENT}>
                        { "Controls" }
                    </SidebarItem>
                </SidebarGroup>
            }
        </Sidebar>
    }
}
