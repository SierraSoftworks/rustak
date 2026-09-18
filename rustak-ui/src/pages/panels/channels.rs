//! One account's channels.
//!
//! `GET`/`PUT /api/v1/users/{username}/groups` is a replacement rather than a
//! change: the whole set is sent, which is what makes the operation idempotent
//! and what lets the page show a picker instead of a list of grants and
//! revocations. So the edits accumulate locally and Save sends what is on
//! screen.

use rustak_api::{GroupMembership, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Card, GroupsPicker, LoadingNote};

use super::super::load::use_resource;

#[derive(Properties, PartialEq)]
pub struct ChannelsPanelProps {
    pub username: Username,
}

#[function_component(ChannelsPanel)]
pub fn channels_panel(props: &ChannelsPanelProps) -> Html {
    let groups = use_resource(api::groups::list);

    let owner = props.username.clone();
    let held = use_resource(move || {
        let owner = owner.clone();
        async move { api::groups::memberships(&owner).await }
    });

    // What is on screen, which is the loaded set until somebody changes it.
    let draft = use_state(|| None::<Vec<GroupMembership>>);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);

    let current = (*draft)
        .clone()
        .unwrap_or_else(|| held.data.clone().unwrap_or_default());

    let onchange = {
        let (draft, saved) = (draft.clone(), saved.clone());
        Callback::from(move |next: Vec<GroupMembership>| {
            saved.set(false);
            draft.set(Some(next));
        })
    };

    let save = {
        let (draft, busy, error, saved) =
            (draft.clone(), busy.clone(), error.clone(), saved.clone());
        let (reload, username) = (held.reload.clone(), props.username.clone());
        let wanted = current.clone();

        Callback::from(move |_: MouseEvent| {
            let (draft, busy, error, saved) =
                (draft.clone(), busy.clone(), error.clone(), saved.clone());
            let (reload, username, wanted) = (reload.clone(), username.clone(), wanted.clone());

            busy.set(true);
            spawn_local(async move {
                match api::groups::set_memberships(&username, &wanted).await {
                    Ok(_) => {
                        error.set(None);
                        saved.set(true);
                        // Drop the draft so the reloaded set — which carries the
                        // memberships the identity provider owns — is what shows.
                        draft.set(None);
                        reload.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let body = match (
        &groups.data,
        &held.data,
        groups.error.as_ref().or(held.error.as_ref()),
    ) {
        (_, _, Some(message)) if groups.data.is_none() || held.data.is_none() => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the channels."
                message={message.clone()}
            />
        },
        (Some(groups), Some(_), _) => html! {
            <>
                <GroupsPicker
                    groups={groups.clone()}
                    value={current}
                    onchange={onchange}
                    disabled={*busy}
                    id_prefix={format!("member-{}", props.username)}
                />

                <div class="channel-picker__actions">
                    <Button
                        kind={ButtonKind::Primary}
                        busy={*busy}
                        disabled={draft.is_none()}
                        title={draft.is_none().then_some("Nothing has changed.")}
                        onclick={save}
                    >
                        { "Save channels" }
                    </Button>
                    if *saved {
                        <span class="channel-picker__saved" role="status">{ "Saved" }</span>
                    }
                </div>
            </>
        },
        _ => html! { <LoadingNote /> },
    };

    html! {
        <Card
            title="Channels"
            subtitle="What this account may write into, and read out of."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="Those channels could not be saved."
                    message={message.clone()}
                />
            }
            { body }
        </Card>
    }
}
