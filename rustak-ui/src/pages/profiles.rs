//! Device profiles: what a client is configured with, and when.
//!
//! The two flags are the whole model and the list leads with them, because
//! "which profile is a phone actually going to get" is the question an
//! operator opens this page with. A profile with neither flag is delivered
//! only when a client asks for it by tool name.
//!
//! Creating one is deliberately small — a name and the flags — because
//! everything that makes a profile worth having is the preferences and files
//! inside it, and those live on its own page.

use rustak_api::{Profile, ProfileCreate, ProfileId};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::app::Route;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, ConfirmButton, Field, LoadingNote, StatusPill,
    StatusTone, Switch, TextInput,
};
use crate::util::{nav_href, short_relative};

use super::load::{use_refresh_action, use_resource};

#[function_component(Profiles)]
pub fn profiles() -> Html {
    let profiles = use_resource(api::profiles::list);
    use_refresh_action(profiles.reload.clone(), profiles.busy);

    let body = match (&profiles.data, &profiles.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the profiles."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "No profiles yet. One marked 'on enrolment' is what every new device is \
                   handed the first time it connects." }
            </p>
        },
        (Some(list), _) => html! {
            <ul class="profile-list">
                { for list.iter().map(|profile| html! {
                    <li key={profile.id.get()}>
                        <ProfileRow
                            profile={profile.clone()}
                            on_changed={profiles.reload.clone()}
                        />
                    </li>
                }) }
            </ul>
        },
    };

    html! {
        <>
            <CreateProfile on_created={profiles.reload.clone()} />

            <Card
                title="All profiles"
                subtitle="Open one to edit its preferences and the files it carries."
            >
                { body }
            </Card>
        </>
    }
}

/// The link from a listed profile to its editor.
///
/// Demo mode lives in the query string and a client-side navigation replaces
/// the whole URL, so a `Link` out of a demo page would land on one talking to
/// a server that is not there.
fn editor_link(profile: &Profile) -> Html {
    let label = profile.name.clone();

    if crate::fixtures::is_demo() {
        let href = nav_href(&format!("/admin/profiles/{}", profile.id.get()));
        return html! { <a class="profile-row__name profile-row__name--link" {href}>{ label }</a> };
    }

    html! {
        <Link<Route>
            to={Route::ProfileEditor { id: profile.id.get() }}
            classes="profile-row__name profile-row__name--link"
        >
            { label }
        </Link<Route>>
    }
}

/// When a profile is delivered, in the words the endpoints use.
fn delivery(profile: &Profile) -> Vec<&'static str> {
    let mut when = Vec::new();

    if profile.apply_on_enrollment {
        when.push("On enrolment");
    }
    if profile.apply_on_connect {
        when.push("On connect");
    }
    if when.is_empty() {
        when.push("By tool name only");
    }

    when
}

#[derive(Properties, PartialEq)]
struct ProfileRowProps {
    profile: Profile,
    on_changed: Callback<()>,
}

#[function_component(ProfileRow)]
fn profile_row(props: &ProfileRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let remove = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let id: ProfileId = props.profile.id;
        Callback::from(move |_| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::profiles::remove(id).await {
                    Ok(()) => {
                        error.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    html! {
        <div class="profile-row">
            <div class="profile-row__identity">
                { editor_link(&props.profile) }
                if let Some(description) = &props.profile.description {
                    <span class="profile-row__description">{ description.clone() }</span>
                }
            </div>

            <div class="profile-row__meta">
                { for delivery(&props.profile).into_iter().map(|when| html! {
                    <span>{ when }</span>
                }) }
                if let Some(tool) = &props.profile.tool {
                    <span title="The tool name a client asks for this profile by">
                        { format!("tool: {tool}") }
                    </span>
                }
                <span title="The channels whose members receive it">
                    { match props.profile.groups.is_empty() {
                        true => "Everybody".to_string(),
                        false => props.profile.groups
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    } }
                </span>
                <span>{ format!("{} preferences", props.profile.pref_count) }</span>
                <span>{ format!("{} files", props.profile.file_count) }</span>
                <span>{ format!("Changed {}", short_relative(props.profile.updated)) }</span>
            </div>

            <StatusPill
                tone={if props.profile.active { StatusTone::Ok } else { StatusTone::Neutral }}
                label={if props.profile.active { "Active" } else { "Inactive" }}
                title={(!props.profile.active)
                    .then_some("Kept and edited, but never handed to a device.")}
            />

            <ConfirmButton
                label="Delete"
                confirm_label="Delete it"
                question={format!(
                    "Delete '{}'? Its preferences and its files go with it, and devices that \
                     already have them keep them.",
                    props.profile.name,
                )}
                busy={*busy}
                onconfirm={remove}
            />

            if let Some(message) = &*error {
                <p class="profile-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct CreateProfileProps {
    on_created: Callback<()>,
}

#[function_component(CreateProfile)]
fn create_profile(props: &CreateProfileProps) -> Html {
    let name = use_state(String::new);
    let description = use_state(String::new);
    let on_enrollment = use_state(|| false);
    let on_connect = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let submit = {
        let (name, description) = (name.clone(), description.clone());
        let (on_enrollment, on_connect) = (on_enrollment.clone(), on_connect.clone());
        let (busy, error, on_created) = (busy.clone(), error.clone(), props.on_created.clone());

        Callback::from(move |_: MouseEvent| {
            if name.trim().is_empty() {
                return;
            }

            let request = ProfileCreate {
                name: name.trim().to_string(),
                description: (!description.trim().is_empty())
                    .then(|| description.trim().to_string()),
                apply_on_enrollment: *on_enrollment,
                apply_on_connect: *on_connect,
                ..ProfileCreate::default()
            };

            let (name, description) = (name.clone(), description.clone());
            let (busy, error, on_created) = (busy.clone(), error.clone(), on_created.clone());

            busy.set(true);
            spawn_local(async move {
                match api::profiles::create(&request).await {
                    Ok(_) => {
                        error.set(None);
                        name.set(String::new());
                        description.set(String::new());
                        on_created.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    html! {
        <Card
            title="Create a profile"
            subtitle="Add its preferences and files once it exists."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That profile could not be created."
                    message={message.clone()}
                />
            }

            <div class="inline-form">
                <Field
                    label="Name"
                    id="profile-name"
                    required=true
                    help="What an operator calls it. Clients never see it."
                >
                    <TextInput
                        id="profile-name"
                        value={(*name).clone()}
                        placeholder="Enrolment defaults"
                        onchange={
                            let name = name.clone();
                            Callback::from(move |value: String| name.set(value))
                        }
                    />
                </Field>

                <Field label="Description" id="profile-description">
                    <TextInput
                        id="profile-description"
                        value={(*description).clone()}
                        placeholder="What this profile is for"
                        onchange={
                            let description = description.clone();
                            Callback::from(move |value: String| description.set(value))
                        }
                    />
                </Field>

                <Field label="Delivered" id="profile-on-enrollment">
                    <Switch
                        id="profile-on-enrollment"
                        checked={*on_enrollment}
                        label="On enrolment"
                        onchange={
                            let on_enrollment = on_enrollment.clone();
                            Callback::from(move |value: bool| on_enrollment.set(value))
                        }
                    />
                    <Switch
                        id="profile-on-connect"
                        checked={*on_connect}
                        label="On connect"
                        onchange={
                            let on_connect = on_connect.clone();
                            Callback::from(move |value: bool| on_connect.set(value))
                        }
                    />
                </Field>

                <div class="inline-form__action">
                    <Button
                        kind={ButtonKind::Primary}
                        busy={*busy}
                        disabled={name.trim().is_empty()}
                        title={name.trim().is_empty().then_some("Give it a name first.")}
                        onclick={submit}
                    >
                        { "Create profile" }
                    </Button>
                </div>
            </div>
        </Card>
    }
}
