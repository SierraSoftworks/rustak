//! Application root: the routes, the authentication gate, and the shared auth
//! context every page reads.

use rustak_api::Me;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api::{self, ApiError};
use crate::auth;
use crate::components::AdminShell;
use crate::pages;

/// The client-side routes handled by the SPA.
#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    /// The public landing page.
    #[at("/")]
    Landing,

    /// Where the identity provider sends the sign-in popup back to, with
    /// `?code&state`. [`use_auth`] completes the exchange on mount.
    #[at("/auth/callback")]
    AuthCallback,

    /// The first-run wizard. Reachable without a session, because on a fresh
    /// server there is nobody to sign in as yet.
    #[at("/setup")]
    Setup,

    /// Both spellings of the admin root are the dashboard.
    #[at("/admin")]
    AdminRoot,
    #[at("/admin/")]
    Dashboard,

    #[at("/admin/devices")]
    Devices,
    #[at("/admin/credentials")]
    Credentials,
    #[at("/admin/users")]
    Users,
    #[at("/admin/groups")]
    Groups,
    #[at("/admin/services")]
    Services,
    #[at("/admin/missions")]
    Missions,
    #[at("/admin/packages")]
    Packages,
    #[at("/admin/profiles")]
    Profiles,
    #[at("/admin/activity")]
    Activity,
    #[at("/admin/settings")]
    Settings,

    /// The control gallery, for reviewing every component without a server. It
    /// exists in debug builds only, alongside the fixtures it renders with.
    #[cfg(debug_assertions)]
    #[at("/demo/controls")]
    DemoControls,

    #[not_found]
    #[at("/404")]
    NotFound,
}

impl Route {
    /// The title and supporting line the shell shows for this route.
    pub fn heading(&self) -> (&'static str, &'static str) {
        match self {
            Route::Devices => (
                "Devices",
                "The EUDs that have connected, and what they came as.",
            ),
            Route::Credentials => (
                "Credentials",
                "Enrolment tokens and client passwords, shown once and revocable.",
            ),
            Route::Users => ("Users", "Everyone who can sign in, and what they may do."),
            Route::Groups => ("Channels", "Who can see whose position reports."),
            Route::Services => ("Services", "The sidecars connected to this server."),
            Route::Missions => ("Missions", "Data Sync missions and their subscribers."),
            Route::Packages => ("Data packages", "The files this server hands out."),
            Route::Profiles => ("Device profiles", "What each device is configured with."),
            Route::Activity => (
                "Activity",
                "What this server has done, and what it refused.",
            ),
            Route::Settings => ("Settings", "How this server describes itself to clients."),
            _ => ("Dashboard", "How this server is doing, at a glance."),
        }
    }
}

/// The resolved authentication state of the application.
#[derive(Clone, PartialEq)]
pub enum AuthStatus {
    /// Still being resolved.
    Loading,

    /// The server has never been set up, so there is nobody to sign in as. The
    /// only useful destination is the wizard.
    NeedsSetup,

    /// Somebody is signed in (or demo mode is active).
    SignedIn(Box<Me>),

    /// A sign-in is required before anything else can happen.
    NeedsLogin,

    /// Refused by the access-control policy. Signing in again cannot change the
    /// outcome, so the UI must not offer it.
    Forbidden,

    /// Resolving the state failed, which is not the same as being refused.
    Error(String),
}

/// The shared authentication handle provided to every page via context.
#[derive(Clone, PartialEq)]
pub struct AuthHandle {
    pub status: AuthStatus,
    pub user: Option<Me>,
    /// Starts the identity provider's popup sign-in.
    pub login: Callback<()>,
    /// Starts a passkey sign-in.
    pub login_passkey: Callback<()>,
    pub signout: Callback<()>,
    /// Re-resolves the session, for the wizard to call once it has one.
    pub refresh: Callback<()>,
}

/// Resolves the current state: the wizard first, then the identity.
///
/// The order matters. A server that has never been set up answers `/me` with a
/// 401 exactly like one whose session expired, and sending somebody to a login
/// page they cannot use is worse than useless — so the setup status is asked
/// for first, and the answer decides which of the two this is.
async fn resolve_status(status: &UseStateHandle<AuthStatus>) {
    match api::setup::status().await {
        Ok(setup) if setup.needs_setup => {
            status.set(AuthStatus::NeedsSetup);
            return;
        }
        // A server that cannot say whether it is set up is still worth trying to
        // sign in to: the identity probe below gives the better error of the two.
        Ok(_) | Err(_) => {}
    }

    match api::auth::me().await {
        Ok(me) => status.set(AuthStatus::SignedIn(Box::new(me))),
        Err(ApiError::Unauthorized) => status.set(AuthStatus::NeedsLogin),
        Err(ApiError::Forbidden) => status.set(AuthStatus::Forbidden),
        Err(err) => status.set(AuthStatus::Error(err.to_string())),
    }
}

/// Resolves the authentication state once on mount and exposes the sign-in,
/// sign-out and re-resolve actions.
#[hook]
fn use_auth() -> AuthHandle {
    let status = use_state(|| AuthStatus::Loading);

    {
        let status = status.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                // Finish any in-flight OIDC callback first: a popup hands its
                // tokens back to the opener and closes here, and a
                // direct-navigation fallback stores them.
                let _ = auth::oidc::complete_callback().await;
                resolve_status(&status).await;
            });
            || ()
        });
    }

    let refresh = {
        let status = status.clone();
        Callback::from(move |_| {
            let status = status.clone();
            status.set(AuthStatus::Loading);
            spawn_local(async move { resolve_status(&status).await });
        })
    };

    let login = {
        let status = status.clone();
        Callback::from(move |_| {
            let status = status.clone();
            spawn_local(async move {
                match auth::oidc::begin_login().await {
                    Ok(Some(_)) => resolve_status(&status).await,
                    // The popup was dismissed without completing; leave the
                    // state as it was rather than inventing a failure.
                    Ok(None) => {}
                    Err(err) => status.set(AuthStatus::Error(err)),
                }
            });
        })
    };

    let login_passkey = {
        let status = status.clone();
        Callback::from(move |_| {
            let status = status.clone();
            spawn_local(async move {
                match auth::passkey::login(None).await {
                    Ok(()) => resolve_status(&status).await,
                    Err(err) => status.set(AuthStatus::Error(err)),
                }
            });
        })
    };

    let signout = {
        let status = status.clone();
        Callback::from(move |_| {
            spawn_local(async { auth::sign_out().await });
            status.set(AuthStatus::NeedsLogin);
        })
    };

    let user = match &*status {
        AuthStatus::SignedIn(me) => Some((**me).clone()),
        _ => None,
    };

    AuthHandle {
        status: (*status).clone(),
        user,
        login,
        login_passkey,
        signout,
        refresh,
    }
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <BrowserRouter>
            <AppInner />
        </BrowserRouter>
    }
}

#[function_component(AppInner)]
fn app_inner() -> Html {
    let auth = use_auth();
    html! {
        <ContextProvider<AuthHandle> context={auth}>
            <Switch<Route> render={switch} />
        </ContextProvider<AuthHandle>>
    }
}

/// Wraps a page in the admin chrome, which also gates it behind the session.
fn admin(page: Html) -> Html {
    html! { <AdminShell>{ page }</AdminShell> }
}

fn switch(route: Route) -> Html {
    match route {
        Route::Landing => html! { <pages::Landing /> },
        Route::AuthCallback => html! { <pages::AuthCallback /> },
        Route::Setup => html! { <pages::Setup /> },
        Route::AdminRoot | Route::Dashboard => admin(html! { <pages::Dashboard /> }),
        Route::Devices => admin(html! { <pages::Devices /> }),
        Route::Credentials => admin(html! { <pages::Credentials /> }),
        Route::Users => admin(html! { <pages::Users /> }),
        Route::Groups => admin(html! { <pages::Groups /> }),
        Route::Services => admin(html! { <pages::Services /> }),
        Route::Missions => admin(html! { <pages::Missions /> }),
        Route::Packages => admin(html! { <pages::Packages /> }),
        Route::Profiles => admin(html! { <pages::Profiles /> }),
        Route::Activity => admin(html! { <pages::Activity /> }),
        Route::Settings => admin(html! { <pages::Settings /> }),
        #[cfg(debug_assertions)]
        Route::DemoControls => html! { <pages::DemoControls /> },
        Route::NotFound => html! { <pages::NotFound /> },
    }
}
