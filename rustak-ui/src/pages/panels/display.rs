//! What the person signed in has chosen for themselves: today, which edition
//! of MIL-STD-2525 their devices author symbols in.
//!
//! The map here needs no such choice. It draws the code each sender wrote, in
//! whichever edition that was, and the 2525C symbol a CoT type implies when
//! the sender wrote none. The choice matters on a *device*, which is where it
//! is delivered: as `symbologyProvider`, with the account's device profile.
//!
//! # Drawn, not written
//!
//! The inputs are drawn from [`rustak_api::preferences::schema`] by the same
//! [`SchemaNode`] that draws a service's configuration, so a preference added
//! to that schema appears here without anybody writing an input for it — its
//! title as the label, its description as the help, its `oneOf` as a picker.
//!
//! These are the account's own preferences rather than the installation's
//! settings, so they sit on the account page, anybody may change theirs, and
//! they follow the account to whichever browser it signs in from. A choice is
//! saved the moment it is made — there is nothing here to get half right — and
//! only what changed is sent. The session is re-resolved afterwards, because
//! that is where every other page reads it from.

use std::rc::Rc;

use rustak_api::UserPreferences;
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card};
use crate::pages::service_config::SchemaNode;

#[derive(Properties, PartialEq)]
pub struct DisplayPanelProps {
    /// What the account has chosen, as the session last reported it.
    pub preferences: UserPreferences,

    /// Emitted once a change has been saved, so the session can be read again.
    pub on_changed: Callback<()>,
}

#[function_component(DisplayPanel)]
pub fn display_panel(props: &DisplayPanelProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let root = use_memo((), |()| rustak_api::preferences::schema());

    let onchange = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let current = props.preferences;

        Callback::from(move |edited: Option<Value>| {
            // A form that reports something the type cannot hold has been
            // cleared or half filled in, and there is nothing to save yet.
            let Some(next) = edited.and_then(|value| serde_json::from_value(value).ok()) else {
                return;
            };
            let patch = current.changes_to(&next);
            if patch.is_empty() {
                return;
            }

            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::auth::set_preferences(&patch).await {
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

    html! {
        <Card
            title="Preferences"
            subtitle="What you have chosen for yourself. These follow your account, to this console and to your devices."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That preference could not be saved."
                    message={message.clone()}
                />
            }

            <SchemaNode
                root={root.clone()}
                schema={(*root).clone()}
                pointer=""
                label=""
                required=true
                value={serde_json::to_value(props.preferences).ok()}
                {onchange}
                issues={Rc::new(Vec::new())}
                disabled={*busy}
                scope="pref"
            />
        </Card>
    }
}
