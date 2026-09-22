//! How the console looks to the person signed in: today, which edition of
//! MIL-STD-2525 the map draws its symbols from.
//!
//! These are the account's own preferences rather than the installation's
//! settings, so they sit on the account page, anybody may change theirs, and
//! they follow the account to whichever browser it signs in from. The choice
//! is saved the moment it is made — there is nothing here to get half right —
//! and the session is re-resolved afterwards, because that is where every
//! other page reads it from.

use rustak_api::{Symbology, UserPreferencesPatch};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, Field, Select, SelectOption};

#[derive(Properties, PartialEq)]
pub struct DisplayPanelProps {
    /// What the account has chosen, as the session last reported it.
    pub symbology: Symbology,

    /// Emitted once a change has been saved, so the session can be read again.
    pub on_changed: Callback<()>,
}

#[function_component(DisplayPanel)]
pub fn display_panel(props: &DisplayPanelProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let onchange = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let current = props.symbology;

        Callback::from(move |chosen: Option<String>| {
            let Some(symbology) = chosen.as_deref().and_then(Symbology::parse) else {
                return;
            };
            if symbology == current {
                return;
            }

            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                let patch = UserPreferencesPatch {
                    symbology: Some(symbology),
                };

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

    let options: Vec<SelectOption> = Symbology::ALL
        .into_iter()
        .map(|edition| SelectOption {
            value: edition.as_str().into(),
            label: edition.label().into(),
        })
        .collect();

    html! {
        <Card
            title="Display"
            subtitle="How the console looks to you. These follow your account, not this browser."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That preference could not be saved."
                    message={message.clone()}
                />
            }

            <Field
                id="pref-symbology"
                label="Map symbols"
                help="Which edition of MIL-STD-2525 the map draws. TAK's types are laid out on \
                    2525C, so every track has a 2525C symbol; 2525D redrew many of them, and a \
                    type with no 2525D equivalent keeps its 2525C symbol."
            >
                <Select
                    id="pref-symbology"
                    value={Some(AttrValue::from(props.symbology.as_str()))}
                    {onchange}
                    {options}
                    disabled={*busy}
                />
            </Field>
        </Card>
    }
}
