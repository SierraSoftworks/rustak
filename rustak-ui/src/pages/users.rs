//! Everybody who can sign in.
//!
//! Two levers, and they are not the same. Suspending an account stops it signing
//! in *and* stops its devices connecting, which is the one thing that works
//! without editing the configuration file. Promoting somebody sets the
//! administrator flag on their row, which the ACL then reads — so it is an
//! override rather than a replacement for `admin_acl`, and the page says which
//! of the two a person's access came from.

use rustak_api::{User, UserPatch, UserSource, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api;
use crate::app::{AuthHandle, Route};
use crate::components::{
    Alert, AlertKind, Card, LoadingNote, MenuAction, MenuItem, SplitButton, StatusPill, StatusTone,
};
use crate::util::{nav_href, optional_relative, urlencode};

use super::load::{use_refresh_action, use_resource};
use super::user_create::CreateUser;

#[function_component(Users)]
pub fn users() -> Html {
    let users = use_resource(api::users::list);
    use_refresh_action(users.reload.clone(), users.busy);

    let body = match (&users.data, &users.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the accounts."
                message={message.clone()}
            />
        },
        (Some(list), error) => html! {
            <>
                if let Some(message) = error {
                    <Alert
                        kind={AlertKind::Warning}
                        title="The accounts could not be refreshed."
                        message={message.clone()}
                    />
                }
                <ul class="user-list">
                    { for list.iter().map(|user| html! {
                        <li key={user.username.to_string()}>
                            <UserRow user={user.clone()} on_changed={users.reload.clone()} />
                        </li>
                    }) }
                </ul>
            </>
        },
    };

    html! {
        <>
            <CreateUser on_created={users.reload.clone()} />
            <Card>{ body }</Card>
        </>
    }
}

/// The link from a row to that account's own page.
///
/// Demo mode lives in the query string and a client-side navigation replaces the
/// whole URL, so following a `Link` out of a demo page would land on one talking
/// to a server that is not there.
fn open(user: &User) -> Html {
    let label = user.display().to_string();

    if crate::fixtures::is_demo() {
        let href = nav_href(&format!(
            "/admin/users/{}",
            urlencode(user.username.as_str())
        ));
        return html! { <a class="user-row__name user-row__name--link" {href}>{ label }</a> };
    }

    html! {
        <Link<Route>
            to={Route::UserDetail { username: user.username.to_string() }}
            classes="user-row__name user-row__name--link"
        >
            { label }
        </Link<Route>>
    }
}

#[derive(Properties, PartialEq)]
struct UserRowProps {
    user: User,
    on_changed: Callback<()>,
}

#[function_component(UserRow)]
fn user_row(props: &UserRowProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // Nobody should be able to lock themselves out of the console they are
    // standing in, so the levers that would do it are not offered.
    let is_self = auth
        .user
        .as_ref()
        .is_some_and(|me| me.username == props.user.username);

    let apply = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let username = props.user.username.clone();
        Callback::from(move |patch: UserPatch| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let username: Username = username.clone();
            busy.set(true);
            spawn_local(async move {
                match api::users::patch(&username, &patch).await {
                    Ok(_) => {
                        error.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let toggle_disabled = {
        let (apply, disabled) = (apply.clone(), props.user.disabled);
        Callback::from(move |()| {
            apply.emit(UserPatch {
                disabled: Some(!disabled),
                ..UserPatch::default()
            })
        })
    };

    let toggle_admin = {
        let (apply, is_admin) = (apply.clone(), props.user.is_admin);
        Callback::from(move |()| {
            apply.emit(UserPatch {
                is_admin: Some(!is_admin),
                ..UserPatch::default()
            })
        })
    };

    let (tone, status) = if props.user.disabled {
        (StatusTone::Neutral, "Suspended")
    } else if props.user.is_admin {
        (StatusTone::Ok, "Administrator")
    } else {
        (StatusTone::Ok, "Active")
    };

    // Where the administrator flag came from, because "admin" set here and
    // "admin" granted by the ACL are undone in different places.
    let admin_source = match (props.user.is_admin, props.user.admin_override) {
        (true, Some(true)) => Some("Set here"),
        (true, _) => Some("From the access-control policy"),
        _ => None,
    };

    html! {
        <div class="user-row">
            <div class="user-row__identity">
                { open(&props.user) }
                <span class="user-row__username">{ props.user.username.to_string() }</span>
            </div>

            <div class="user-row__meta">
                <span>{ props.user.kind.label() }</span>
                <span>{ props.user.source.label() }</span>
                <span title="When this account was last seen">
                    { optional_relative(props.user.last_seen_at) }
                </span>
            </div>

            <StatusPill {tone} label={status} title={admin_source} />

            // One button on the row: the flag change on its face, and the
            // suspension — the one that locks somebody out — a click further.
            <SplitButton
                busy={*busy}
                menu_label={format!("More actions for {}", props.user.username)}
                primary={MenuAction::new(
                    if props.user.is_admin { "Demote" } else { "Promote" },
                    toggle_admin,
                )
                .disabled(is_self || props.user.source == UserSource::Service)
                .title(is_self.then_some("You cannot change your own administrator flag here."))}
                items={vec![MenuItem::Action({
                    let suspend = MenuAction::new(
                        if props.user.disabled { "Restore" } else { "Suspend" },
                        toggle_disabled,
                    )
                    .disabled(is_self)
                    .title(is_self.then_some("You cannot suspend your own account."));
                    if props.user.disabled { suspend } else { suspend.danger() }
                })]}
            />

            if let Some(message) = &*error {
                <p class="user-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}
