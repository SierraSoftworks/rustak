//! One account's own details.
//!
//! Three levers and they are not the same thing. The display name is ours to
//! set unless the identity provider owns the account, in which case it is
//! rewritten at the member's next sign-in. The administrator flag is an
//! *override* that wins over the `admin_acl` expression, so the panel says which
//! of the two a person's access came from. Suspending stops the account signing
//! in and stops its devices connecting, which is the one lever that works
//! without editing the configuration file.

use rustak_api::{User, UserPatch, UserSource};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, StatusPill, StatusTone, Switch, TextInput,
};
use crate::util::{format_iso8601, optional_relative};

#[derive(Properties, PartialEq)]
pub struct ProfilePanelProps {
    pub user: User,
    pub on_changed: Callback<()>,
}

#[function_component(ProfilePanel)]
pub fn profile_panel(props: &ProfilePanelProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let name = use_state(|| None::<String>);

    // Nobody should be able to lock themselves out of the console they are
    // standing in, so the levers that would do it are not offered.
    let is_self = auth
        .user
        .as_ref()
        .is_some_and(|me| me.username == props.user.username);

    let display_name = (*name)
        .clone()
        .unwrap_or_else(|| props.user.display_name.clone().unwrap_or_default());

    let on_name = {
        let name = name.clone();
        Callback::from(move |value: String| name.set(Some(value)))
    };

    let apply = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let (name, username) = (name.clone(), props.user.username.clone());
        Callback::from(move |patch: UserPatch| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let (name, username) = (name.clone(), username.clone());
            busy.set(true);
            spawn_local(async move {
                match api::users::patch(&username, &patch).await {
                    Ok(_) => {
                        error.set(None);
                        name.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let save_name = {
        let (apply, display_name) = (apply.clone(), display_name.clone());
        Callback::from(move |_: MouseEvent| {
            apply.emit(UserPatch {
                display_name: Some(display_name.clone()),
                ..UserPatch::default()
            })
        })
    };

    let toggle_admin = {
        let (apply, is_admin) = (apply.clone(), props.user.is_admin);
        Callback::from(move |_: bool| {
            apply.emit(UserPatch {
                is_admin: Some(!is_admin),
                ..UserPatch::default()
            })
        })
    };

    let toggle_disabled = {
        let (apply, disabled) = (apply.clone(), props.user.disabled);
        Callback::from(move |_: bool| {
            apply.emit(UserPatch {
                disabled: Some(!disabled),
                ..UserPatch::default()
            })
        })
    };

    let managed_here = props.user.source.is_managed_here();

    html! {
        <Card title="Profile" subtitle="Who this is, and what they may do.">
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That change could not be saved."
                    message={message.clone()}
                />
            }

            if !managed_here {
                <Alert
                    kind={AlertKind::Info}
                    title="This account comes from your identity provider."
                    message="Its display name, email address and channel memberships are \
                        refreshed from the provider's claims every time it signs in, so a \
                        change made here lasts until then and no longer."
                />
            }

            <dl class="detail-list profile-panel__facts">
                <dt>{ "Username" }</dt>
                <dd class="detail-list__mono">{ props.user.username.to_string() }</dd>

                <dt>{ "Email" }</dt>
                <dd>
                    { props.user.email.clone().unwrap_or_else(|| "—".to_string()) }
                </dd>

                <dt>{ "Kind" }</dt>
                <dd>{ props.user.kind.label() }</dd>

                <dt>{ "Source" }</dt>
                <dd>{ props.user.source.label() }</dd>

                <dt>{ "Created" }</dt>
                <dd title={format_iso8601(props.user.created_at)}>
                    { optional_relative(Some(props.user.created_at)) }
                </dd>

                <dt>{ "Last seen" }</dt>
                <dd>{ optional_relative(props.user.last_seen_at) }</dd>

                <dt>{ "Access" }</dt>
                <dd>
                    <StatusPill
                        tone={if props.user.disabled { StatusTone::Neutral } else { StatusTone::Ok }}
                        label={if props.user.disabled { "Suspended" } else { "Active" }}
                        title={admin_source(&props.user)}
                    />
                </dd>
            </dl>

            <Field
                label="Display name"
                id="profile-display-name"
                help="What this account is called everywhere in the console."
            >
                <TextInput
                    id="profile-display-name"
                    value={display_name.clone()}
                    disabled={*busy}
                    onchange={on_name}
                />
            </Field>

            <div class="profile-panel__toggles">
                <Switch
                    id="profile-admin"
                    label="Administrator"
                    checked={props.user.is_admin}
                    disabled={*busy || is_self || props.user.source == UserSource::Service}
                    onchange={toggle_admin}
                />
                <Switch
                    id="profile-enabled"
                    label="Enabled"
                    checked={!props.user.disabled}
                    disabled={*busy || is_self}
                    onchange={toggle_disabled}
                />
            </div>

            if is_self {
                <p class="profile-panel__note">
                    { "You cannot change your own administrator flag or suspend your own \
                       account here." }
                </p>
            }

            <Button
                kind={ButtonKind::Primary}
                busy={*busy}
                disabled={name.is_none()}
                title={name.is_none().then_some("Nothing has changed.")}
                onclick={save_name}
            >
                { "Save profile" }
            </Button>
        </Card>
    }
}

/// Where the administrator flag came from, because "admin" set here and "admin"
/// granted by the access-control policy are undone in different places.
fn admin_source(user: &User) -> Option<&'static str> {
    match (user.is_admin, user.admin_override) {
        (true, Some(true)) => Some("Administrator: set here"),
        (true, _) => Some("Administrator: from the access-control policy"),
        _ => None,
    }
}
