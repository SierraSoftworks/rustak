//! The chrome shared by every admin view.
//!
//! It renders the app bar, the sidebar navigation, the page's own title row
//! with a slot the page can push actions into, and gates the routed page behind
//! the session — so a page only mounts, and therefore only fetches, once access
//! has been granted.

use std::cell::Cell;

use chrono::{Datelike, Utc};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::app::Route;
use crate::components::{AppBar, PageTitle};
use crate::fixtures;
use crate::pages::Protected;
use crate::util::{narrow_screen, nav_href, window};

#[derive(Properties, PartialEq)]
pub struct AdminShellProps {
    #[prop_or_default]
    pub children: Html,
}

/// A slot for page-specific actions at the end of the title row.
///
/// Pages take it from context and push controls (a refresh button, a "new"
/// button) into the shared header without owning the title itself.
#[derive(Clone, PartialEq)]
pub struct PageActions {
    set: Callback<Html>,
}

impl PageActions {
    pub fn set(&self, actions: Html) {
        self.set.emit(actions);
    }

    pub fn clear(&self) {
        self.set.emit(Html::default());
    }
}

#[function_component(AdminShell)]
pub fn admin_shell(props: &AdminShellProps) -> Html {
    // The action slot. Its setter is memoised so the context identity stays
    // stable and pages do not re-render each other when one changes its actions.
    let actions = use_state(Html::default);
    let page_actions = {
        let actions = actions.clone();
        use_memo((), move |_| PageActions {
            set: Callback::from(move |content: Html| actions.set(content)),
        })
    };

    // Whether the navigation drawer is open. Only meaningful on a narrow
    // screen, where the sidebar is hidden until asked for.
    let nav_open = use_state(|| false);

    // Whether the sidebar is folded away. Only meaningful on a wide screen,
    // where it is otherwise always there — and remembered, because somebody
    // who wants the width for a map wants it next time too.
    let nav_folded = use_state(stored_fold);

    // The shell is shared, so what it is a shell *for* has to come from the
    // route. Hard-coding one page's title here would make every other page claim
    // to be the dashboard.
    let route = use_route::<Route>().unwrap_or(Route::Dashboard);
    let (title, subtitle) = route.heading();

    // Every other page is a column of cards that reads best at a fixed
    // measure, under a title. A map is the one thing here that is better for
    // every pixel: it takes the whole main area, and what the title row would
    // have said is laid over it.
    let immersive = route == Route::Map;

    // A page's actions belong to the page. Clearing them on every route change
    // stops the previous page's refresh button outliving it — and following a
    // link is what closes the drawer, so the page it opened can be seen.
    {
        let (actions, nav_open) = (actions.clone(), nav_open.clone());
        use_effect_with(route.clone(), move |_| {
            actions.set(Html::default());
            nav_open.set(false);
            || ()
        });
    }

    // Which of the two the navigation is depends on the width, and the width
    // changes under a page that is already drawn: a tablet is turned, a
    // window is halved. Crossing the line draws the shell again, so the
    // button says what it will do, and shuts a drawer that would otherwise
    // be left open over a page that no longer has one.
    {
        let (nav_open, redraw) = (nav_open.clone(), use_force_update());
        use_effect_with((), move |_| {
            let was_narrow = Cell::new(narrow_screen());
            let on_resize = Closure::<dyn Fn()>::new(move || {
                let narrow = narrow_screen();
                if was_narrow.replace(narrow) != narrow {
                    nav_open.set(false);
                    redraw.force_update();
                }
            });
            let _ = window()
                .add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref());

            move || {
                let _ = window().remove_event_listener_with_callback(
                    "resize",
                    on_resize.as_ref().unchecked_ref(),
                );
            }
        });
    }

    // One button, and what it does depends on what the navigation is at this
    // width: a drawer to open, or a column to fold.
    let toggle_nav = {
        let (nav_open, nav_folded) = (nav_open.clone(), nav_folded.clone());
        Callback::from(move |_| {
            if narrow_screen() {
                nav_open.set(!*nav_open);
            } else {
                store_fold(!*nav_folded);
                nav_folded.set(!*nav_folded);
            }
        })
    };
    let drawer = narrow_screen();
    let nav_shown = if drawer { *nav_open } else { !*nav_folded };
    let close_nav = {
        let nav_open = nav_open.clone();
        Callback::from(move |_| nav_open.set(false))
    };

    html! {
        <div class="app-shell">
            <AppBar menu_open={nav_shown} {drawer} on_menu={toggle_nav} />
            <div class="app-body">
                <AdminNav open={*nav_open} folded={*nav_folded} on_close={close_nav} />
                <main class="app-main">
                    <div class={classes!("app-container", immersive.then_some("app-container--immersive"))}>
                        <ContextProvider<PageActions> context={(*page_actions).clone()}>
                            <Protected>
                                if immersive {
                                    // The page is still headed, for whoever is
                                    // reading it rather than looking at it.
                                    <h1 class="visually-hidden">{ title }</h1>
                                } else {
                                    <PageTitle title={title} subtitle={subtitle}>
                                        { (*actions).clone() }
                                    </PageTitle>
                                }
                                { props.children.clone() }
                            </Protected>
                        </ContextProvider<PageActions>>
                    </div>
                    if !immersive {
                        <footer class="app-footer">
                            <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
                        </footer>
                    }
                </main>
            </div>
        </div>
    }
}

/// Where the fold is remembered between visits.
const FOLD_KEY: &str = "rustak.admin.nav_folded";

fn stored_fold() -> bool {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(FOLD_KEY).ok().flatten())
        .is_some_and(|value| value == "1")
}

fn store_fold(folded: bool) {
    if let Some(storage) = window().local_storage().ok().flatten() {
        // A browser that refuses storage still folds; it only forgets.
        let _ = match folded {
            true => storage.set_item(FOLD_KEY, "1"),
            false => storage.remove_item(FOLD_KEY),
        };
    }
}

/// One section of the navigation: what it is for, and where it goes.
struct NavGroup {
    title: &'static str,
    links: &'static [(Route, &'static str)],
}

/// Every destination, grouped by what somebody is there to do: what is
/// happening, who and what connects, what they exchange, and how the server
/// itself is set up.
const NAV: &[NavGroup] = &[
    NavGroup {
        title: "Overview",
        links: &[
            (Route::Dashboard, "Dashboard"),
            (Route::Map, "Map"),
            (Route::Situation, "Situation"),
            (Route::Activity, "Activity"),
        ],
    },
    NavGroup {
        title: "Clients",
        links: &[
            (Route::Users, "Users"),
            (Route::Euds, "EUDs"),
            (Route::Profiles, "Profiles"),
        ],
    },
    NavGroup {
        title: "Operations",
        links: &[
            (Route::Channels, "Channels"),
            (Route::Missions, "Missions"),
            (Route::Packages, "Packages"),
        ],
    },
    NavGroup {
        title: "Settings",
        links: &[
            (Route::Account, "Account"),
            (Route::Security, "Security"),
            (Route::Storage, "Storage"),
            (Route::Services, "Services"),
        ],
    },
];

#[derive(Properties, PartialEq)]
struct AdminNavProps {
    /// Whether the drawer is slid in, on a screen narrow enough to have one.
    open: bool,
    /// Whether the column is folded away, on a screen wide enough to have one.
    folded: bool,
    on_close: Callback<()>,
}

/// The links between the admin pages, down the side of every one of them.
#[function_component(AdminNav)]
fn admin_nav(props: &AdminNavProps) -> Html {
    let current = use_route::<Route>().unwrap_or(Route::Dashboard);

    let link = |route: &Route, label: &'static str| {
        // Both spellings of the dashboard are the same destination, so one must
        // not appear unselected while the other is showing.
        let active = match (route, &current) {
            (Route::Dashboard, Route::Dashboard | Route::AdminRoot) => true,
            // One account's page is somewhere inside Users, so the list must
            // not read as though nothing is selected while it is open. The
            // same for a mission and a profile, which are rows on their lists.
            (Route::Users, Route::Users | Route::UserDetail { .. }) => true,
            (Route::Missions, Route::Missions | Route::MissionDetail { .. }) => true,
            (Route::Profiles, Route::Profiles | Route::ProfileEditor { .. }) => true,
            (route, current) => route == current,
        };

        let classes = classes!(
            "admin-nav__link",
            active.then_some("admin-nav__link--active")
        );

        // Demo mode lives in the query string and a client-side navigation
        // replaces the whole URL, so following a link out of a demo page would
        // otherwise land on one talking to a server that is not there.
        if fixtures::is_demo() {
            return html! {
                <a class={classes} href={nav_href(&route.to_path())}>{ label }</a>
            };
        }

        html! { <Link<Route> to={route.clone()} classes={classes}>{ label }</Link<Route>> }
    };

    let group = |group: &NavGroup| {
        html! {
            <div class="admin-nav__group">
                <div class="admin-nav__heading">{ group.title }</div>
                { for group.links.iter().map(|(route, label)| link(route, label)) }
            </div>
        }
    };

    let on_backdrop = {
        let on_close = props.on_close.clone();
        Callback::from(move |_: MouseEvent| on_close.emit(()))
    };

    html! {
        <>
            if props.open {
                <div class="sidebar__backdrop" aria-hidden="true" onclick={on_backdrop} />
            }
            <aside
                id="admin-nav"
                class={classes!(
                    "sidebar",
                    props.open.then_some("sidebar--open"),
                    props.folded.then_some("sidebar--folded"),
                )}
            >
                <div class="sidebar__inner">
                    <nav class="admin-nav" aria-label="Admin sections">
                        { for NAV.iter().map(group) }
                        // Only reachable in demo mode, which is the only mode it
                        // works in.
                        if fixtures::is_demo() {
                            <div class="admin-nav__group">
                                <div class="admin-nav__heading">{ "Development" }</div>
                                <a class="admin-nav__link" href={nav_href("/demo/controls")}>
                                    { "Controls" }
                                </a>
                            </div>
                        }
                    </nav>
                </div>
            </aside>
        </>
    }
}
