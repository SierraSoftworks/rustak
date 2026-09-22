//! The configuration an administrator sets for one sidecar.
//!
//! `GET/PUT /api/v1/services/{name}/config` is a JSON object: the
//! administrator writes it, the service reads its own and nobody else's. What
//! its keys are is the plugin's business, so what this panel offers depends on
//! what the plugin said about them when it registered:
//!
//! | The service registered… | The panel is… |
//! |---|---|
//! | a JSON Schema | a form drawn from it ([`form`], [`schema`]) — every key named, every value an input of the right kind, the plugin's doc comments as help text — with the JSON one click away |
//! | nothing | the JSON text box it always was ([`json`]) |
//!
//! # A save is checked before it is stored
//!
//! `POST …/config/validate` first. The server holds the candidate to the schema
//! and, when the service is running, asks the service itself — which is the
//! only thing that knows whether an API key works. What either objects to is
//! shown beside the input it is about, and nothing is stored. A service that
//! cannot be asked is not an error: the save goes ahead on the schema's word,
//! and the panel says that is what happened.
//!
//! # Why it re-reads before it writes
//!
//! `PUT` is a replacement, not a merge, and this console polls: two
//! administrators — or one administrator and a browser left open on the page
//! since yesterday — can each be editing a copy that is no longer what the
//! server holds. So a save first reads what is stored, and refuses when that
//! is not what this panel loaded. The alternative is a silent overwrite of
//! somebody's settings, which is the one failure here nobody would notice.

mod form;
mod groups;
mod inputs;
mod json;
mod schema;

use std::rc::Rc;

use rustak_api::{ConfigValidationReport, ServiceCheck};
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Card, Field, LoadingNote, TextArea};

use super::load::use_resource;
use form::SchemaNode;
use json::{as_text, config_problem};

/// What to say, after a save, about whether the service itself was asked.
fn service_note(report: &ConfigValidationReport) -> Option<&'static str> {
    match report.service {
        ServiceCheck::Checked => Some("The running service checked it before it was stored."),
        ServiceCheck::Unreachable => Some(
            "The service could not be reached to check it, so only its schema was applied. \
             Watch its health after its next tick.",
        ),
        ServiceCheck::NotSupported | ServiceCheck::Skipped => None,
    }
}

/// What a validation objected to, as a list that names where.
fn issues(report: &ConfigValidationReport) -> Html {
    let lead = match report.service {
        ServiceCheck::Checked => "The service itself checked it, and objected:",
        _ => "It does not match what this service says it accepts:",
    };

    html! {
        <>
            <Alert
                kind={AlertKind::Error}
                title="That configuration was not saved."
                message={lead.to_string()}
            />
            <ul class="config-form__issues">
                { for report.issues.iter().map(|issue| html! {
                    <li>
                        if let Some(path) = &issue.path {
                            <code>{ path }</code>{ ": " }
                        }
                        { &issue.message }
                    </li>
                }) }
            </ul>
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct ServiceConfigProps {
    /// The service's own name, which is what the route is keyed on.
    pub name: AttrValue,

    /// The JSON Schema it registered for its configuration, if it did.
    #[prop_or_default]
    pub schema: Option<Value>,
}

#[function_component(ServiceConfigPanel)]
pub fn service_config_panel(props: &ServiceConfigProps) -> Html {
    let name = props.name.to_string();
    let stored = use_resource({
        let name = name.clone();
        move || async move { api::services::config(&name).await }
    });

    // The text being edited, and the document it was loaded from. The text is
    // the one source of truth — the form reads and writes it too — and the
    // second is what the save compares against, so "changed since load" is a
    // fact about the server rather than about this panel.
    let draft = use_state(String::new);
    let loaded = use_state(|| None::<Value>);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);
    let report = use_state(|| None::<ConfigValidationReport>);
    let as_json = use_state(|| false);
    let root = use_memo(props.schema.clone(), |schema| {
        schema.clone().unwrap_or_default()
    });

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

    // Any edit: what was said about the last candidate is no longer about this one.
    let edit = {
        let (draft, saved, report) = (draft.clone(), saved.clone(), report.clone());
        Callback::from(move |text: String| {
            saved.set(false);
            report.set(None);
            draft.set(text);
        })
    };

    let save = {
        let name = name.clone();
        let (draft, loaded, busy, error, saved, report) = (
            draft.clone(),
            loaded.clone(),
            busy.clone(),
            error.clone(),
            saved.clone(),
            report.clone(),
        );

        Callback::from(move |_: MouseEvent| {
            let Ok(wanted) = serde_json::from_str::<Value>(&draft) else {
                return;
            };

            let name = name.clone();
            let (draft, loaded, busy, error, saved, report) = (
                draft.clone(),
                loaded.clone(),
                busy.clone(),
                error.clone(),
                saved.clone(),
                report.clone(),
            );

            busy.set(true);
            saved.set(false);
            error.set(None);
            spawn_local(async move {
                let outcome = async {
                    // Asked twice. Before the check, so that nobody waits on a
                    // validation that is already moot; and again after it,
                    // because the running service may take ten seconds to
                    // answer and somebody else may have saved by then.
                    still_as_loaded(&name, loaded.as_ref()).await?;

                    let checked = api::services::validate_config(&name, &wanted).await?;
                    let written = match checked.valid {
                        true => {
                            still_as_loaded(&name, loaded.as_ref()).await?;
                            Some(api::services::set_config(&name, &wanted).await?)
                        }
                        false => None,
                    };

                    Ok::<_, api::ApiError>((checked, written))
                };

                match outcome.await {
                    Ok((checked, written)) => {
                        if let Some(written) = written {
                            draft.set(as_text(&written));
                            loaded.set(Some(written));
                            saved.set(true);
                        }
                        report.set(Some(checked));
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let problem = config_problem(&draft);
    let changed = loaded.as_ref().map(as_text).as_deref() != Some(draft.as_str());
    let document = serde_json::from_str::<Value>(&draft)
        .ok()
        .filter(|_| props.schema.is_some() && !*as_json && problem.is_none());

    let fields = match document {
        Some(document) => html! {
            <SchemaNode
                root={root.clone()}
                schema={(*root).clone()}
                pointer=""
                label=""
                required=true
                value={document}
                onchange={edit.reform(|value: Option<Value>| as_text(&value.unwrap_or_default()))}
                issues={Rc::new(report.as_ref().map(|r| r.issues.clone()).unwrap_or_default())}
                disabled={*busy}
            />
        },
        None => html! {
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
                    onchange={edit.clone()}
                />
            </Field>
        },
    };

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
                if props.schema.is_some() {
                    <div class="config-form__toolbar">
                        <Button
                            kind={ButtonKind::Subtle}
                            small=true
                            // Text that is not an object has no form to go back to.
                            disabled={*busy || (*as_json && problem.is_some())}
                            onclick={
                                let as_json = as_json.clone();
                                Callback::from(move |_: MouseEvent| as_json.set(!*as_json))
                            }
                        >
                            { if *as_json { "Edit as a form" } else { "Edit as JSON" } }
                        </Button>
                    </div>
                }

                { fields }

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
                    if let Some(note) = report.as_ref().and_then(service_note) {
                        <p class="panel-note">{ note }</p>
                    }
                }
            </>
        },
    };

    html! {
        <Card
            title="Configuration"
            subtitle={match props.schema {
                Some(_) => "Drawn from the schema this service registered. Only an administrator can write it.",
                None => "A JSON object this service reads. Only an administrator can write it.",
            }}
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That configuration was not saved."
                    message={message.clone()}
                />
            }
            if let Some(refused) = report.as_ref().filter(|report| !report.valid) {
                { issues(refused) }
            }

            { editor }
        </Card>
    }
}

/// Refuses when the stored configuration is no longer the one this editor
/// loaded, so that a save never silently undoes somebody else's.
async fn still_as_loaded(name: &str, loaded: Option<&Value>) -> Result<(), api::ApiError> {
    if Some(&api::services::config(name).await?) == loaded {
        return Ok(());
    }

    Err(api::ApiError::Server(
        "This service's configuration changed somewhere else while you were editing it. Reload \
         the page to see what it says now, then make your change again."
            .to_string(),
    ))
}
