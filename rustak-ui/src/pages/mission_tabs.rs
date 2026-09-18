//! The two lists a mission's detail document already carries: who is
//! subscribed, and how its contents are arranged.
//!
//! The change log is not here — see `mission_changes`, which fetches its own.

use rustak_api::{
    MissionDetail, MissionGuid, MissionLayerSummary, MissionRoleKind, MissionSubscriptionSummary,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Card, ConfirmButton, Select, StatusPill, StatusTone, role_options};
use crate::util::{format_iso8601, short_relative};

#[derive(Properties, PartialEq)]
pub struct MissionSubscribersProps {
    pub mission: MissionDetail,
    pub on_changed: Callback<()>,
}

/// Who is subscribed, what each may do, and taking one off.
#[function_component(MissionSubscribers)]
pub fn mission_subscribers(props: &MissionSubscribersProps) -> Html {
    let guid = props.mission.summary.guid;

    html! {
        <Card
            title="Subscribers"
            subtitle="Every device receiving this mission, and what it may do to it."
        >
            if props.mission.subscriptions.is_empty() {
                <p class="panel-empty">
                    { "Nobody is subscribed. A client subscribes from its own map, or is \
                       invited to." }
                </p>
            } else {
                <ul class="subscriber-list">
                    { for props.mission.subscriptions.iter().map(|held| html! {
                        <li key={held.client_uid.clone()}>
                            <SubscriberRow
                                {guid}
                                subscription={held.clone()}
                                on_changed={props.on_changed.clone()}
                            />
                        </li>
                    }) }
                </ul>
            }
        </Card>
    }
}

#[derive(Properties, PartialEq)]
struct SubscriberRowProps {
    guid: MissionGuid,
    subscription: MissionSubscriptionSummary,
    on_changed: Callback<()>,
}

#[function_component(SubscriberRow)]
fn subscriber_row(props: &SubscriberRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_role = {
        let (guid, uid) = (props.guid, props.subscription.client_uid.clone());
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |chosen: Option<String>| {
            let Some(role) = chosen.as_deref().and_then(MissionRoleKind::parse) else {
                return;
            };

            let uid = uid.clone();
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());

            busy.set(true);
            spawn_local(async move {
                match api::missions::set_role(&guid, &uid, role).await {
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

    let remove = {
        let (guid, uid) = (props.guid, props.subscription.client_uid.clone());
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |_| {
            let uid = uid.clone();
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::missions::unsubscribe(&guid, &uid).await {
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

    let held = &props.subscription;

    html! {
        <div class="subscriber-row">
            <div class="subscriber-row__identity">
                <span class="subscriber-row__name">
                    { held.username.clone().unwrap_or_else(|| "Unknown account".to_string()) }
                </span>
                <span class="subscriber-row__uid">{ held.client_uid.clone() }</span>
            </div>

            <div class="subscriber-row__meta">
                <span title={format_iso8601(held.create_time)}>
                    { format!("Subscribed {}", short_relative(held.create_time)) }
                </span>
            </div>

            <StatusPill
                tone={if held.connected { StatusTone::Ok } else { StatusTone::Neutral }}
                label={if held.connected { "Connected" } else { "Offline" }}
                title={(!held.connected).then_some(
                    "Still subscribed — it will be sent what it missed when it reconnects.",
                )}
            />

            <Select
                id={format!("subscriber-role-{}", held.client_uid)}
                value={Some(AttrValue::from(held.role.as_str()))}
                options={role_options()}
                disabled={*busy}
                onchange={on_role}
            />

            <ConfirmButton
                label="Remove"
                confirm_label="Remove it"
                question={format!(
                    "Remove '{}'? It keeps what it has already synced; this stops it being \
                     sent more.",
                    held.client_uid,
                )}
                busy={*busy}
                onconfirm={remove}
            />

            if let Some(message) = &*error {
                <p class="subscriber-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct MissionLayersProps {
    pub layers: Vec<MissionLayerSummary>,
}

/// The layer tree, nested by `parent_uid`.
#[function_component(MissionLayers)]
pub fn mission_layers(props: &MissionLayersProps) -> Html {
    html! {
        <Card title="Layers" subtitle="How this mission's contents are arranged for a client.">
            if props.layers.is_empty() {
                <p class="panel-empty">
                    { "No layers. Everything in this mission sits at its root." }
                </p>
            } else {
                { branch(&props.layers, None) }
            }
        </Card>
    }
}

/// One level of the tree, in the order the server gave.
///
/// Recursion rather than a flattening pass: a layer tree is a handful of nodes
/// deep at most, and the alternative — a depth field on a flat list — would
/// have to be recomputed on every render anyway.
fn branch(layers: &[MissionLayerSummary], parent: Option<&str>) -> Html {
    let children: Vec<&MissionLayerSummary> = layers
        .iter()
        .filter(|layer| layer.parent_uid.as_deref() == parent)
        .collect();

    if children.is_empty() {
        return html! {};
    }

    html! {
        <ul class="layer-tree">
            { for children.into_iter().map(|layer| html! {
                <li key={layer.uid.clone()}>
                    <div class="layer-node">
                        <span class="layer-node__name">
                            { layer.name.clone().unwrap_or_else(|| layer.uid.clone()) }
                        </span>
                        <span class="layer-node__kind">{ layer.kind.clone() }</span>
                        <span class="layer-node__count">
                            { format!("{} items", layer.item_count) }
                        </span>
                    </div>
                    { branch(layers, Some(&layer.uid)) }
                </li>
            }) }
        </ul>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_can_be_offered_in_the_dropdown() {
        assert_eq!(role_options().len(), MissionRoleKind::ALL.len());
    }
}
