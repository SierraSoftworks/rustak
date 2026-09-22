//! What the person signed in has chosen for themselves: today, which edition
//! of MIL-STD-2525 their devices author symbols in.
//!
//! The map here needs no such choice. It draws the code each sender wrote, in
//! whichever edition that was, and the 2525C symbol a CoT type implies when
//! the sender wrote none. The choice matters on a *device*, which is where it
//! is delivered: as `symbologyProvider`, with the account's device profile.
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

            <Field
                id="pref-symbology"
                label="Symbol edition"
                help="Which edition of MIL-STD-2525 your devices author symbols in. It reaches \
                    each device with its profile, the next time it connects. The map here draws \
                    every symbol in the edition its sender wrote it in."
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
