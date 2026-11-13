//! One channel, opened from its row: its description, who is in it, and the
//! button that deletes it.
//!
//! Renaming is not here because it cannot be done (see [`super::groups`]), and
//! a channel that came from the configuration file or the identity provider is
//! shown with its description read-only: it is not an administrator's to
//! change, though who is in it still is.

use rustak_api::{Group, GroupPatch};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, ConfirmButton, DetailDrawer, Field, TextInput,
};

use super::group_members::GroupMembers;

#[derive(Properties, PartialEq)]
pub struct ChannelDrawerProps {
    pub group: Group,

    /// The description was saved or the channel deleted, so the list is stale.
    pub on_changed: Callback<()>,

    pub on_close: Callback<()>,
}

#[function_component(ChannelDrawer)]
pub fn channel_drawer(props: &ChannelDrawerProps) -> Html {
    let group = &props.group;
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let description = use_state(|| None::<String>);

    let current = (*description)
        .clone()
        .unwrap_or_else(|| group.description.clone().unwrap_or_default());

    let save = {
        let (busy, error, description) = (busy.clone(), error.clone(), description.clone());
        let (name, on_changed, value) = (
            group.name.clone(),
            props.on_changed.clone(),
            current.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            let (busy, error, description) = (busy.clone(), error.clone(), description.clone());
            let (name, on_changed) = (name.clone(), on_changed.clone());
            let change = GroupPatch {
                description: Some(value.clone()),
            };

            busy.set(true);
            spawn_local(async move {
                match api::groups::patch(&name, &change).await {
                    Ok(_) => {
                        error.set(None);
                        description.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let remove = {
        let (busy, error) = (busy.clone(), error.clone());
        let (on_changed, on_close) = (props.on_changed.clone(), props.on_close.clone());
        let name = group.name.clone();
        Callback::from(move |_| {
            let (busy, error) = (busy.clone(), error.clone());
            let (on_changed, on_close) = (on_changed.clone(), on_close.clone());
            let name = name.clone();

            busy.set(true);
            spawn_local(async move {
                match api::groups::remove(&name).await {
                    Ok(()) => {
                        on_changed.emit(());
                        on_close.emit(());
                    }
                    Err(err) => {
                        error.set(Some(err.to_string()));
                        busy.set(false);
                    }
                }
            });
        })
    };

    let editable = group.source.is_editable();
    let field = format!("channel-description-{}", group.bitpos);

    let actions = html! {
        <>
            if editable {
                <ConfirmButton
                    small=false
                    label="Delete"
                    confirm_label="Delete it"
                    question={format!(
                        "Delete '{}'? Everyone in it loses it, and the name cannot be reused \
                         with the same bit position.",
                        group.name,
                    )}
                    busy={*busy}
                    onconfirm={remove}
                />
            }
            <Button
                kind={ButtonKind::Primary}
                busy={*busy}
                disabled={description.is_none() || !editable}
                title={description.is_none().then_some("Nothing has changed.")}
                onclick={save}
            >
                { "Save" }
            </Button>
        </>
    };

    html! {
        <DetailDrawer
            title={group.name.to_string()}
            subtitle={format!("{} · bit {}", group.source.label(), group.bitpos)}
            {actions}
            on_close={props.on_close.clone()}
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That change could not be made."
                    message={message.clone()}
                />
            }

            <Field
                label="Description"
                id={field.clone()}
                help={(!editable).then_some("This channel is not an administrator's to change.")}
            >
                <TextInput
                    id={field}
                    value={current}
                    disabled={*busy || !editable}
                    placeholder="No description"
                    onchange={
                        let description = description.clone();
                        Callback::from(move |value: String| description.set(Some(value)))
                    }
                />
            </Field>

            <GroupMembers group={group.clone()} />
        </DetailDrawer>
    }
}
