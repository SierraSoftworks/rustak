//! The configuration an administrator sets for one sidecar.
//!
//! `GET/PUT /api/v1/services/{name}/config` is a JSON object: the
//! administrator writes it, the service reads its own and nobody else's. It is
//! edited as text rather than as a form because there is no schema behind it —
//! every plugin decides what its own keys are — so the panel's job is to
//! refuse what the server would refuse, and to be honest about when the change
//! takes effect, which is the sidecar's next tick and not this request.
//!
//! # Why it re-reads before it writes
//!
//! `PUT` is a replacement, not a merge, and this console polls: two
//! administrators — or one administrator and a browser left open on the page
//! since yesterday — can each be editing a copy that is no longer what the
//! server holds. So a save first reads what is stored, and refuses when that
//! is not what this panel loaded. The alternative is a silent overwrite of
//! somebody's settings, which is the one failure here nobody would notice.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Card, Field, LoadingNote, TextArea};

use super::load::use_resource;

/// What is wrong with the text somebody typed, if anything.
///
/// The same two rules the server applies, applied before the request rather
/// than after it: it has to be JSON, and it has to be an object.
pub fn config_problem(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return Some(
            "A configuration is a JSON object. Write {} for a service with nothing to set."
                .to_string(),
        );
    }

    match serde_json::from_str::<serde_json::Value>(text) {
        Err(err) => Some(format!("That is not JSON we can read: {err}.")),
        Ok(value) if !value.is_object() => Some(
            "A service's configuration has to be a JSON object — a set of keys, wrapped in { }."
                .to_string(),
        ),
        Ok(_) => None,
    }
}

/// The stored configuration as the editor shows it.
fn as_text(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[derive(Properties, PartialEq)]
pub struct ServiceConfigProps {
    /// The service's own name, which is what the route is keyed on.
    pub name: AttrValue,
}

#[function_component(ServiceConfigPanel)]
pub fn service_config_panel(props: &ServiceConfigProps) -> Html {
    let name = props.name.to_string();
    let stored = use_resource({
        let name = name.clone();
        move || async move { api::services::config(&name).await }
    });

    // The text being edited, and the document it was loaded from. The second
    // is what the save compares against, so "changed since load" is a fact
    // about the server rather than about this textarea.
    let draft = use_state(String::new);
    let loaded = use_state(|| None::<serde_json::Value>);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);

    {
        let (draft, loaded) = (draft.clone(), loaded.clone());
        use_effect_with(stored.data.clone(), move |data| {
            if let Some(value) = data {
                draft.set(as_text(value));
                loaded.set(Some(value.clone()));
            }
            || ()
        });
    }

    let save = {
        let name = name.clone();
        let (draft, loaded, busy, error, saved) = (
            draft.clone(),
            loaded.clone(),
            busy.clone(),
            error.clone(),
            saved.clone(),
        );

        Callback::from(move |_: MouseEvent| {
            let text = (*draft).clone();
            if let Some(problem) = config_problem(&text) {
                error.set(Some(problem));
                saved.set(false);
                return;
            }

            let Ok(wanted) = serde_json::from_str::<serde_json::Value>(&text) else {
                return;
            };

            let name = name.clone();
            let (draft, loaded, busy, error, saved) = (
                draft.clone(),
                loaded.clone(),
                busy.clone(),
                error.clone(),
                saved.clone(),
            );

            busy.set(true);
            saved.set(false);
            spawn_local(async move {
                match api::services::config(&name).await {
                    Ok(current) if Some(&current) != loaded.as_ref() => {
                        error.set(Some(
                            "This service's configuration changed somewhere else while you were \
                             editing it. Reload the page to see what it says now, then make your \
                             change again."
                                .to_string(),
                        ));
                    }
                    Ok(_) => match api::services::set_config(&name, &wanted).await {
                        Ok(written) => {
                            draft.set(as_text(&written));
                            loaded.set(Some(written));
                            error.set(None);
                            saved.set(true);
                        }
                        Err(err) => error.set(Some(err.to_string())),
                    },
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let problem = config_problem(&draft);
    let changed = loaded.as_ref().map(as_text).as_deref() != Some(draft.as_str());

    let editor = match (&stored.data, &stored.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read this service's configuration."
                message={message.clone()}
            />
        },
        (Some(_), _) => html! {
            <>
                <Field
                    label="JSON document"
                    id="service-config"
                    help="A JSON object. Every key in it is this plugin's own."
                    error={problem.clone().map(AttrValue::from)}
                >
                    <TextArea
                        id="service-config"
                        value={(*draft).clone()}
                        rows={12}
                        monospace=true
                        invalid={problem.is_some()}
                        disabled={*busy}
                        onchange={
                            let (draft, saved) = (draft.clone(), saved.clone());
                            Callback::from(move |value: String| {
                                saved.set(false);
                                draft.set(value);
                            })
                        }
                    />
                </Field>

                <p class="panel-note">
                    { "The service reads this on its next tick, so a change here reaches a \
                       running sidecar without anybody restarting it." }
                </p>

                <Button
                    kind={ButtonKind::Primary}
                    busy={*busy}
                    disabled={problem.is_some() || !changed}
                    onclick={save}
                >
                    { "Save configuration" }
                </Button>

                if *saved && !changed {
                    <p class="panel-note panel-note--ok" role="status">
                        { "Saved. The service picks it up on its next tick." }
                    </p>
                }
            </>
        },
    };

    html! {
        <Card
            title="Configuration"
            subtitle="A JSON object this service reads. Only an administrator can write it."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That configuration was not saved."
                    message={message.clone()}
                />
            }

            { editor }
        </Card>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_json_object_is_a_configuration() {
        assert_eq!(config_problem(r#"{ "interval_seconds": 30 }"#), None);
        assert_eq!(config_problem("{}"), None);

        assert!(
            config_problem("   ")
                .expect("a blank box is not an object")
                .contains("{}")
        );
        assert!(
            config_problem("{ oops }")
                .expect("that is not JSON")
                .starts_with("That is not JSON we can read")
        );
        // Valid JSON, but not something the server would store.
        assert!(
            config_problem("[1, 2, 3]")
                .expect("an array is not an object")
                .contains("JSON object")
        );
        assert!(config_problem("42").is_some());
    }

    #[test]
    fn the_editor_shows_the_stored_document_rather_than_one_long_line() {
        let text = as_text(&serde_json::json!({ "interval_seconds": 30 }));

        assert!(text.contains('\n'), "{text}");
        assert_eq!(config_problem(&text), None);
    }
}
