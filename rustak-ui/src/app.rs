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

    /// The end-user devices: enrolled, and whether each is connected now.
    #[at("/admin/euds")]
    Euds,
    #[at("/admin/users")]
    Users,
    /// One account, with its devices, credentials and channels.
    #[at("/admin/users/:username")]
    UserDetail { username: String },
    #[at("/admin/channels")]
    Channels,
    #[at("/admin/missions")]
    Missions,
    /// One Data Sync mission, addressed by guid because a name may be renamed
    /// and may itself be a bare UUID.
    #[at("/admin/missions/:guid")]
    MissionDetail { guid: String },
    #[at("/admin/packages")]
    Packages,
    /// Everything that is reporting, drawn where it says it is and kept
    /// current while the page is open.
    #[at("/admin/map")]
    Map,
    /// The latest situational-awareness message per identifier.
    #[at("/admin/situation")]
    Situation,
    #[at("/admin/profiles")]
    Profiles,
    /// One device profile, with its preferences and its files.
    #[at("/admin/profiles/:id")]
    ProfileEditor { id: i64 },
    #[at("/admin/activity")]
    Activity,

    /// The signed-in account's own page: how they sign in, and what they hold.
    /// It needs no administrative access, which is what lets a person enrol
    /// their own phone.
    #[at("/admin/settings/account")]
    Account,
    /// How this server is reached and how people prove who they are.
    #[at("/admin/settings/security")]
    Security,
    /// What this server keeps, and how much it accepts.
    #[at("/admin/settings/storage")]
    Storage,
    /// The sidecars registered with this server, and what they are reporting.
    ///
    /// Still at `/admin/settings/add-ons`: the page was called Add-ons until
    /// the sidecars it lists existed, and a path that changes breaks every
    /// bookmark somebody made of it.
    #[at("/admin/settings/add-ons")]
    Services,

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
            Route::Euds => (
                "EUDs",
                "The end-user devices that have enrolled, and which are connected right now.",
            ),
            Route::Users => ("Users", "Everyone who can sign in, and what they may do."),
            Route::UserDetail { .. } => (
                "Account",
                "One account: who they are, what they carry, and what they can see.",
            ),
            Route::Channels => ("Channels", "Who can see whose position reports."),
            Route::Missions => ("Missions", "Data Sync missions and their subscribers."),
            Route::MissionDetail { .. } => (
                "Mission",
                "One Data Sync mission: who is on it, what changed, and how it is arranged.",
            ),
            Route::Packages => ("Data packages", "The files this server hands out."),
            Route::Map => ("Map", "Where everything is, as it reports it."),
            Route::Situation => (
                "Situation",
                "Every entity this server knows of, where it last was, and when it last spoke.",
            ),
            Route::Profiles => ("Device profiles", "What each device is configured with."),
            Route::ProfileEditor { .. } => (
                "Device profile",
                "One profile: when it is delivered, to whom, and what it carries.",
            ),
            Route::Activity => (
                "Activity",
                "What this server has done, and what it refused.",
            ),
            Route::Account => (
                "Your account",
                "How you sign in, and the credentials and devices you hold.",
            ),
            Route::Security => (
                "Security",
                "How this server is reached, and how people prove who they are.",
            ),
            Route::Storage => (
                "Storage",
                "What this server keeps, and how much it will accept.",
            ),
            Route::Services => (
                "Services",
                "The sidecars registered with this server, and what each last reported.",
            ),
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
    /// Why the last sign-in attempt did not sign anybody in. Distinct from
    /// [`AuthStatus::Error`], which is about not being able to tell who
    /// somebody is: this is the server (or the provider) having said no, and
    /// the prompt shows it beside the buttons so the person can try another way.
    pub login_error: Option<String>,
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
    let login_error = use_state(|| None::<String>);

    {
        let (status, login_error) = (status.clone(), login_error.clone());
        use_effect_with((), move |_| {
            spawn_local(async move {
                // Finish any in-flight OIDC callback first: a popup hands its
                // outcome back to the opener and closes here, and a
                // direct-navigation fallback stores the tokens — or, when the
                // server refused, has a reason to show on the prompt.
                if let Err(message) = auth::oidc::complete_callback().await {
                    login_error.set(Some(message));
                }
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
        let (status, login_error) = (status.clone(), login_error.clone());
        Callback::from(move |_| {
            let (status, login_error) = (status.clone(), login_error.clone());
            login_error.set(None);
            spawn_local(async move {
                match auth::oidc::begin_login().await {
                    Ok(Some(_)) => resolve_status(&status).await,
                    // The popup was dismissed without completing; leave the
                    // state as it was rather than inventing a failure.
                    Ok(None) => {}
                    Err(err) => login_error.set(Some(err)),
                }
            });
        })
    };

    let login_passkey = {
        let (status, login_error) = (status.clone(), login_error.clone());
        Callback::from(move |_| {
            let (status, login_error) = (status.clone(), login_error.clone());
            login_error.set(None);
            spawn_local(async move {
                match auth::passkey::login(None).await {
                    Ok(()) => resolve_status(&status).await,
                    Err(err) => login_error.set(Some(err)),
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
        login_error: (*login_error).clone(),
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
        Route::Euds => admin(html! { <pages::Euds /> }),
        Route::Users => admin(html! { <pages::Users /> }),
        Route::UserDetail { username } => admin(html! { <pages::UserDetail {username} /> }),
        Route::Channels => admin(html! { <pages::Groups /> }),
        Route::Missions => admin(html! { <pages::Missions /> }),
        Route::MissionDetail { guid } => admin(html! { <pages::MissionDetailPage {guid} /> }),
        Route::Packages => admin(html! { <pages::Packages /> }),
        Route::Map => admin(html! { <pages::LiveMap /> }),
        Route::Situation => admin(html! { <pages::Situation /> }),
        Route::Profiles => admin(html! { <pages::Profiles /> }),
        Route::ProfileEditor { id } => admin(html! { <pages::ProfileEditor {id} /> }),
        Route::Activity => admin(html! { <pages::Activity /> }),
        // The signed-in account's own, because that is what the server answers
        // when a credentials request names nobody — so this page needs no
        // administrative access and a person can enrol their own phone.
        Route::Account => admin(html! { <pages::Me /> }),
        Route::Security => admin(html! { <pages::Security /> }),
        Route::Storage => admin(html! { <pages::Storage /> }),
        Route::Services => admin(html! { <pages::Services /> }),
        #[cfg(debug_assertions)]
        Route::DemoControls => html! { <pages::DemoControls /> },
        Route::NotFound => html! { <pages::NotFound /> },
    }
}
