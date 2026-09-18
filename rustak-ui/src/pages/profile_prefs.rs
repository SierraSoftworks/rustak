//! The preferences card on a profile's page.
//!
//! Its own file because it is its own endpoint: `PUT /profiles/{id}/prefs`
//! replaces the whole list, so this card has a draft, a Save and a failure of
//! its own, separate from the profile row beside it.
//!
//! The draft is not sent as it is typed. A `.pref` document is imported by a
//! device as a unit, and a list saved halfway through being edited is a list
//! that would be delivered halfway through being edited.

use rustak_api::{PrefEntry, ProfileId};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonGroup, ButtonKind, Card, LoadingNote, PrefsEditor, problem_with,
};

use super::load::use_resource;

#[derive(Properties, PartialEq)]
pub struct ProfilePrefsProps {
    pub id: ProfileId,
}

#[function_component(ProfilePrefs)]
pub fn profile_prefs(props: &ProfilePrefsProps) -> Html {
    let id = props.id;
    let stored = use_resource(move || async move { api::profiles::prefs(id).await });
    let catalog = use_resource(api::profiles::pref_catalog);

    let draft = use_state(Vec::<PrefEntry>::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);

    // What the server holds is the source of truth. Re-reading after a save —
    // or after somebody else's — replaces the draft rather than being
    // overwritten by it.
    //
    // `saved` is deliberately *not* cleared here. A save reloads, which lands
    // in this effect, so clearing it would take the confirmation away in the
    // same breath as earning it. It is the draft diverging that makes the
    // notice stale, and `dirty` below is what says so.
    {
        let draft = draft.clone();
        use_effect_with(stored.data.clone(), move |entries| {
            if let Some(entries) = entries {
                draft.set(entries.clone());
            }
            || ()
        });
    }

    let dirty = stored.data.as_ref().is_some_and(|held| *held != *draft);
    let problem = problem_with(&draft);

    let save = {
        let (draft, reload) = (draft.clone(), stored.reload.clone());
        let (busy, error, saved) = (busy.clone(), error.clone(), saved.clone());

        Callback::from(move |_: MouseEvent| {
            let entries = (*draft).clone();
            let (busy, error, saved, reload) =
                (busy.clone(), error.clone(), saved.clone(), reload.clone());

            busy.set(true);
            spawn_local(async move {
                match api::profiles::set_prefs(id, &entries).await {
                    Ok(_) => {
                        error.set(None);
                        saved.set(true);
                        reload.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let revert = {
        let (draft, stored) = (draft.clone(), stored.data.clone());
        Callback::from(move |_: MouseEvent| {
            if let Some(entries) = &stored {
                draft.set(entries.clone());
            }
        })
    };

    let body = match (&stored.data, &stored.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the preferences."
                message={message.clone()}
            />
        },
        (Some(_), _) => html! {
            <PrefsEditor
                value={(*draft).clone()}
                catalog={catalog.data.clone().unwrap_or_default()}
                disabled={*busy}
                onchange={
                    let draft = draft.clone();
                    Callback::from(move |entries: Vec<PrefEntry>| draft.set(entries))
                }
            />
        },
    };

    html! {
        <Card
            title="Preferences"
            subtitle="Delivered in this order, each with the Java class ATAK reads it as."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="Those preferences could not be saved."
                    message={message.clone()}
                />
            } else if *saved && !dirty {
                <Alert kind={AlertKind::Success} title="Saved." />
            }

            { body }

            if stored.data.is_some() {
                <ButtonGroup>
                    <Button
                        kind={ButtonKind::Primary}
                        busy={*busy}
                        disabled={!dirty || problem.is_some()}
                        title={problem.clone().or_else(|| (!dirty)
                            .then(|| "Nothing has changed.".to_string()))}
                        onclick={save}
                    >
                        { "Save preferences" }
                    </Button>

                    <Button
                        disabled={!dirty || *busy}
                        title={(!dirty).then_some("Nothing has changed.")}
                        onclick={revert}
                    >
                        { "Revert" }
                    </Button>
                </ButtonGroup>
            }

            if let Some(problem) = &problem {
                <p class="pref-row__note pref-row__note--error" role="alert">
                    { problem.clone() }
                </p>
            }
        </Card>
    }
}
