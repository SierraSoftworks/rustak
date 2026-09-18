//! One profile: when it is delivered, to whom, and what it carries.
//!
//! Three cards rather than one form, because the three are three endpoints
//! with three failure modes: the profile row is a `PATCH`, the preferences are
//! a whole-list `PUT`, and each file is its own upload. A single Save over all
//! three would either half-apply or have to be undone, and neither is
//! something a page can do honestly.
//!
//! The Preview button is what makes the page trustworthy: it asks the server
//! to assemble the package a device would actually receive, by the same code
//! path as the real delivery — so what an operator opens is what a client
//! would import, rather than a rendering of this page's own state.

use rustak_api::{Profile, ProfileId};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::app::Route;
use crate::components::{Alert, AlertKind, LoadingNote};
use crate::util::{format_iso8601, nav_href};

use super::load::{use_refresh_action, use_resource};
use super::profile_delivery::Settings;
use super::profile_files::ProfileFiles;
use super::profile_prefs::ProfilePrefs;

#[derive(Properties, PartialEq)]
pub struct ProfileEditorProps {
    /// The profile's identifier, straight out of the route.
    pub id: i64,
}

#[function_component(ProfileEditor)]
pub fn profile_editor(props: &ProfileEditorProps) -> Html {
    let id = ProfileId::new(props.id);
    let profile = use_resource(move || async move { api::profiles::get(id).await });
    use_refresh_action(profile.reload.clone(), profile.busy);

    match (&profile.data, &profile.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not open that profile."
                message={message.clone()}
            />
        },
        (Some(found), _) => html! {
            <>
                <Heading profile={found.clone()} />
                <Settings profile={found.clone()} on_changed={profile.reload.clone()} />
                <ProfilePrefs {id} />
                <ProfileFiles {id} on_changed={profile.reload.clone()} />
            </>
        },
    }
}

#[derive(Properties, PartialEq)]
struct HeadingProps {
    profile: Profile,
}

#[function_component(Heading)]
fn heading(props: &HeadingProps) -> Html {
    html! {
        <div class="entity-heading">
            <div>
                <h2 class="entity-heading__name">{ props.profile.name.clone() }</h2>
                <p class="entity-heading__meta">
                    { format!(
                        "{} preferences · {} files · changed {}",
                        props.profile.pref_count,
                        props.profile.file_count,
                        format_iso8601(props.profile.updated),
                    ) }
                </p>
            </div>
            { back_link() }
        </div>
    }
}

/// Demo mode lives in the query string, which a client-side navigation would
/// drop — so the way back out of a demo page is a plain link that carries it.
fn back_link() -> Html {
    if crate::fixtures::is_demo() {
        return html! {
            <a class="btn btn--small" href={nav_href("/admin/profiles")}>{ "All profiles" }</a>
        };
    }

    html! {
        <Link<Route> to={Route::Profiles} classes="btn btn--small">{ "All profiles" }</Link<Route>>
    }
}
