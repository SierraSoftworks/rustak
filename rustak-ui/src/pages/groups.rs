//! Channels: what this server routes by.
//!
//! The list says what each channel is; its description, its members and the
//! button that deletes it open in a drawer from its row
//! ([`super::channel_drawer`]).
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

use rustak_api::{CreateGroupRequest, Group, GroupName};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, CreateDrawer, Field, LoadingNote, Section, StatusPill,
    StatusTone, TextInput,
};

use super::channel_drawer::ChannelDrawer;
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
                        { channel_row(
                            group,
                            selected.as_deref() == Some(group.name.as_str()),
                            {
                                let (selected, name) = (selected.clone(), group.name.to_string());
                                Callback::from(move |_: MouseEvent| selected.set(Some(name.clone())))
                            },
                        ) }
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

            <Section title="All channels" subtitle="Open one to describe it and change who is in it.">
                { body }
            </Section>

            if let Some(group) = chosen {
                // Keyed on the name so that another channel is a new drawer
                // rather than the last one's half-typed description.
                <ChannelDrawer
                    key={group.name.to_string()}
                    group={group.clone()}
                    on_changed={groups.reload.clone()}
                    on_close={
                        let selected = selected.clone();
                        Callback::from(move |()| selected.set(None))
                    }
                />
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
        <CreateDrawer
            button="New channel"
            busy={*busy}
            failed={error.is_some()}
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
        </CreateDrawer>
    }
}

/// One channel's row: what it is, and the way into its drawer.
fn channel_row(group: &Group, selected: bool, onselect: Callback<MouseEvent>) -> Html {
    let editable = group.source.is_editable();

    html! {
        <div class={classes!("channel-row", selected.then_some("channel-row--selected"))}>
            <button type="button" class="channel-row__select" onclick={onselect}>
                <span class="channel-row__name">{ group.name.to_string() }</span>
                <span class="channel-row__bitpos" title="The bit position the router uses">
                    { format!("bit {}", group.bitpos) }
                </span>
            </button>

            <span class="channel-row__description">
                { group.description.clone().unwrap_or_default() }
            </span>

            <StatusPill
                tone={if editable { StatusTone::Ok } else { StatusTone::Neutral }}
                label={group.source.label()}
                title={(!editable).then_some("This channel is not an administrator's to change.")}
            />
        </div>
    }
}
