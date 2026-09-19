//! The upload ceiling, and what the TAK surface tells clients about itself.
//!
//! One card because they are one question — "what does Enterprise Sync do on
//! this installation" — and two endpoints because only one of them is a
//! setting anybody may change from here.
//!
//! # The limit is a limit
//!
//! The same number is advertised to clients through `/files/api/config` and
//! enforced inside every upload reader, so changing it changes what a client
//! is *told* as well as what it may send. CloudTAK's setup wizard reads it
//! before it will save a connection, which is why the two must not disagree.
//!
//! # The configuration file wins
//!
//! A `PUT` while `config.toml` pins the value answers `409` rather than
//! storing something the next start would override. The field is therefore
//! disabled, with the reason stated, rather than offering an edit that would
//! be refused.

use rustak_api::FileSettings;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Card, Field, LoadingNote, NumberInput,
};

use super::load::use_resource;

#[function_component(FilesCard)]
pub fn files_card() -> Html {
    let files = use_resource(api::settings::files);
    let marti = use_resource(api::settings::marti);

    let marti = match &marti.data {
        None => html! {},
        Some(marti) => html! {
            <dl class="detail-list">
                <dt>{ "Public host" }</dt>
                <dd>
                    { marti.public_host.clone().unwrap_or_else(
                        || "— inferred from the request —".to_string(),
                    ) }
                </dd>

                <dt>{ "Cross-origin" }</dt>
                <dd>
                    { match marti.allow_all_origins {
                        true => "Any origin may read a Marti response.",
                        false => "Same origin only.",
                    } }
                </dd>
            </dl>
        },
    };

    // The loaded card is `UploadLimit`'s own to render: the button in its
    // heading belongs to the draft that lives there.
    let body = match (&files.data, &files.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the file settings."
                message={message.clone()}
            />
        },
        (Some(settings), _) => {
            return html! {
                <UploadLimit settings={*settings} on_changed={files.reload.clone()}>
                    { marti }
                </UploadLimit>
            };
        }
    };

    html! {
        <FilesCardFrame>
            { body }
            { marti }
        </FilesCardFrame>
    }
}

#[derive(Properties, PartialEq)]
struct FilesCardFrameProps {
    #[prop_or_default]
    actions: Html,
    #[prop_or_default]
    children: Html,
}

/// The card itself, so the loading, failed and loaded states share one title.
#[function_component(FilesCardFrame)]
fn files_card_frame(props: &FilesCardFrameProps) -> Html {
    html! {
        <Card
            title="Enterprise Sync"
            subtitle="What clients may upload, and what this server tells them about itself."
            actions={props.actions.clone()}
        >
            { props.children.clone() }
        </Card>
    }
}

#[derive(Properties, PartialEq)]
struct UploadLimitProps {
    settings: FileSettings,
    on_changed: Callback<()>,
    /// The rest of the card, under the field.
    #[prop_or_default]
    children: Html,
}

#[function_component(UploadLimit)]
fn upload_limit(props: &UploadLimitProps) -> Html {
    let draft = use_state(|| Some(i64::from(props.settings.upload_size_limit_mb)));
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    {
        let draft = draft.clone();
        let stored = props.settings.upload_size_limit_mb;
        use_effect_with(stored, move |stored| {
            draft.set(Some(i64::from(*stored)));
            || ()
        });
    }

    let pinned = props.settings.from_config_file;
    let wanted = draft.filter(|value| *value > 0);
    let changed =
        wanted.is_some_and(|value| value != i64::from(props.settings.upload_size_limit_mb));

    let save = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        Callback::from(move |_: MouseEvent| {
            let Some(value) = wanted.and_then(|value| u32::try_from(value).ok()) else {
                return;
            };

            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::settings::set_files(value).await {
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

    let actions = html! {
        <Button
            small=true
            kind={ButtonKind::Primary}
            busy={*busy}
            disabled={pinned || !changed}
            title={match (pinned, changed) {
                (true, _) => Some("The configuration file pins this value."),
                (false, false) => Some("Nothing has changed."),
                _ => None,
            }}
            onclick={save}
        >
            { "Save limit" }
        </Button>
    };

    html! {
        <FilesCardFrame {actions}>
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That limit could not be saved."
                    message={message.clone()}
                />
            }

            <Field
                label="Upload limit (MB)"
                id="files-upload-limit"
                help={match pinned {
                    true => "Set in config.toml, which wins — the server refuses a change here.",
                    false => "Advertised to clients and enforced on every upload, so the two \
                              cannot disagree.",
                }}
            >
                <NumberInput
                    id="files-upload-limit"
                    value={*draft}
                    min=1
                    disabled={pinned || *busy}
                    invalid={draft.is_some_and(|value| value <= 0)}
                    onchange={
                        let draft = draft.clone();
                        Callback::from(move |value: Option<i64>| draft.set(value))
                    }
                />
            </Field>

            { props.children.clone() }
        </FilesCardFrame>
    }
}
