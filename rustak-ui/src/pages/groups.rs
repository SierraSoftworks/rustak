//! Channels: what this server routes by.
//!
//! A channel is a routing decision rather than a document somebody owns, so
//! everything here is administrative. Two things it deliberately cannot do:
//!
//! - **Rename.** Every membership, every `groups` claim and every client's
//!   cached selection refers to a channel by name, so renaming one is deleting
//!   it and making another. `GroupPatch` carries only the description, and the
//!   form says so rather than offering a field the server would refuse.
//! - **Choose a bit position.** It is the server's to allocate and never reused
//!   while anything is still subscribed, because a reused one would hand this
//!   channel's traffic to the next channel created. It is shown, read-only.

use rustak_api::{CreateGroupRequest, Group, GroupName, GroupPatch};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, LoadingNote, MenuAction, MenuItem,
    SplitButton, StatusPill, StatusTone, TextInput,
};

use super::group_members::GroupMembers;
use super::load::{use_refresh_action, use_resource};

#[function_component(Groups)]
pub fn groups() -> Html {
    let groups = use_resource(api::groups::list);
    use_refresh_action(groups.reload.clone(), groups.busy);

    let selected = use_state(|| None::<String>);

    let body = match (&groups.data, &groups.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the channels."
                message={message.clone()}
            />
        },
        (Some(list), _) => html! {
            <ul class="channel-list">
                { for list.iter().map(|group| html! {
                    <li key={group.id.get()}>
                        <ChannelRow
                            group={group.clone()}
                            selected={selected.as_deref() == Some(group.name.as_str())}
                            onselect={
                                let (selected, name) = (selected.clone(), group.name.to_string());
                                Callback::from(move |_: MouseEvent| {
                                    selected.set((selected.as_deref() != Some(name.as_str()))
                                        .then(|| name.clone()));
                                })
                            }
                            on_changed={groups.reload.clone()}
                        />
                    </li>
                }) }
            </ul>
        },
    };

    let chosen = groups.data.as_ref().and_then(|list| {
        list.iter()
            .find(|group| selected.as_deref() == Some(group.name.as_str()))
            .cloned()
    });

    html! {
        <>
            <CreateChannel on_created={groups.reload.clone()} />

            <Card title="All channels" subtitle="Select one to see and change who is in it.">
                { body }
            </Card>

            if let Some(group) = chosen {
                <GroupMembers {group} />
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
struct CreateChannelProps {
    on_created: Callback<()>,
}

#[function_component(CreateChannel)]
fn create_channel(props: &CreateChannelProps) -> Html {
    let name = use_state(String::new);
    let description = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let parsed = GroupName::parse(&name);
    let invalid = (!name.trim().is_empty())
        .then(|| parsed.as_ref().err().map(|err| err.to_string()))
        .flatten();

    let submit = {
        let (name, description) = (name.clone(), description.clone());
        let (busy, error, on_created) = (busy.clone(), error.clone(), props.on_created.clone());

        Callback::from(move |_: MouseEvent| {
            let Ok(parsed) = GroupName::parse(&name) else {
                return;
            };
            let (name, description) = (name.clone(), description.clone());
            let (busy, error, on_created) = (busy.clone(), error.clone(), on_created.clone());

            let request = CreateGroupRequest {
                name: parsed,
                description: (!description.trim().is_empty())
                    .then(|| description.trim().to_string()),
            };

            busy.set(true);
            spawn_local(async move {
                match api::groups::create(&request).await {
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

    let actions = html! {
        <Button
            small=true
            kind={ButtonKind::Primary}
            busy={*busy}
            disabled={parsed.is_err()}
            title={parsed.is_err().then_some("Give it a valid name first.")}
            onclick={submit}
        >
            { "Create channel" }
        </Button>
    };

    html! {
        <Card
            title="Create a channel"
            subtitle="The bit position is allocated by the server."
            {actions}
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That channel could not be created."
                    message={message.clone()}
                />
            }

            <div class="inline-form">
                <Field
                    label="Name"
                    id="channel-name"
                    required=true
                    error={invalid.clone().map(AttrValue::from)}
                    help="Clients see this name, and it cannot be changed afterwards."
                >
                    <TextInput
                        id="channel-name"
                        value={(*name).clone()}
                        placeholder="Blue Team"
                        invalid={invalid.is_some()}
                        onchange={
                            let name = name.clone();
                            Callback::from(move |value: String| name.set(value))
                        }
                    />
                </Field>

                <Field label="Description" id="channel-description">
                    <TextInput
                        id="channel-description"
                        value={(*description).clone()}
                        placeholder="What this channel is for"
                        onchange={
                            let description = description.clone();
                            Callback::from(move |value: String| description.set(value))
                        }
                    />
                </Field>
            </div>
        </Card>
    }
}

#[derive(Properties, PartialEq)]
struct ChannelRowProps {
    group: Group,
    selected: bool,
    onselect: Callback<MouseEvent>,
    on_changed: Callback<()>,
}

#[function_component(ChannelRow)]
fn channel_row(props: &ChannelRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let description = use_state(|| None::<String>);

    let current = (*description)
        .clone()
        .unwrap_or_else(|| props.group.description.clone().unwrap_or_default());

    let save = {
        let (busy, error, description) = (busy.clone(), error.clone(), description.clone());
        let (name, on_changed, value) = (
            props.group.name.clone(),
            props.on_changed.clone(),
            current.clone(),
        );
        Callback::from(move |()| {
            let (busy, error, description) = (busy.clone(), error.clone(), description.clone());
            let (name, on_changed, value) = (name.clone(), on_changed.clone(), value.clone());
            let change = GroupPatch {
                description: Some(value),
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
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let name = props.group.name.clone();
        Callback::from(move |_| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let name = name.clone();
            busy.set(true);
            spawn_local(async move {
                match api::groups::remove(&name).await {
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

    let editable = props.group.source.is_editable();

    html! {
        <div class={classes!("channel-row", props.selected.then_some("channel-row--selected"))}>
            <button
                type="button"
                class="channel-row__select"
                aria-pressed={props.selected.to_string()}
                onclick={props.onselect.clone()}
            >
                <span class="channel-row__name">{ props.group.name.to_string() }</span>
                <span class="channel-row__bitpos" title="The bit position the router uses">
                    { format!("bit {}", props.group.bitpos) }
                </span>
            </button>

            <StatusPill
                tone={if editable { StatusTone::Ok } else { StatusTone::Neutral }}
                label={props.group.source.label()}
                title={(!editable).then_some("This channel is not an administrator's to change.")}
            />

            <TextInput
                id={format!("channel-description-{}", props.group.bitpos)}
                value={current}
                disabled={*busy || !editable}
                placeholder="No description"
                onchange={
                    let description = description.clone();
                    Callback::from(move |value: String| description.set(Some(value)))
                }
            />

            // Saving the description is what the box beside it is for, so it
            // is the face; deleting the channel is behind the caret, and asks.
            <SplitButton
                busy={*busy}
                menu_label={format!("More actions for {}", props.group.name)}
                primary={MenuAction::new("Save", save)
                    .disabled(description.is_none() || !editable)
                    .title(description.is_none().then_some("Nothing has changed."))}
                items={vec![MenuItem::Action(
                    MenuAction::new("Delete", remove)
                        .danger()
                        .disabled(!editable)
                        .title((!editable).then_some("Only a channel created here can be deleted."))
                        .confirm(
                            format!(
                                "Delete '{}'? Everyone in it loses it, and the name cannot be \
                                 reused with the same bit position.",
                                props.group.name,
                            ),
                            "Delete it",
                        ),
                )]}
            />

            if let Some(message) = &*error {
                <p class="channel-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}
