//! The files card on a profile's page.
//!
//! A profile file is stored under the path a device will write it to, not
//! under whatever it happened to be called on somebody's desktop — a map
//! source is only a map source if it lands in `maps/`. So the drop zone has a
//! path box beside it, prefilled from the file's own name, and the server
//! refuses anything that could climb out of the profile.

use rustak_api::{ProfileFile, ProfileId};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Card, ConfirmButton, Field, FileDrop, LoadingNote, TextInput,
};
use crate::util::short_relative;

use super::load::use_resource;

/// How a size is worth reading on a row.
fn size_label(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} kB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

#[derive(Properties, PartialEq)]
pub struct ProfileFilesProps {
    pub id: ProfileId,

    /// Lets the page above re-read the counts on the profile row.
    pub on_changed: Callback<()>,
}

#[function_component(ProfileFiles)]
pub fn profile_files(props: &ProfileFilesProps) -> Html {
    let id = props.id;
    let files = use_resource(move || async move { api::profiles::files(id).await });

    let path = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let onfiles = {
        let (path, busy, error) = (path.clone(), busy.clone(), error.clone());
        let (reload, on_changed) = (files.reload.clone(), props.on_changed.clone());

        Callback::from(move |chosen: Vec<web_sys::File>| {
            let Some(file) = chosen.into_iter().next() else {
                return;
            };

            let wanted = (*path).clone();
            let (path, busy, error) = (path.clone(), busy.clone(), error.clone());
            let (reload, on_changed) = (reload.clone(), on_changed.clone());

            busy.set(true);
            spawn_local(async move {
                let name = (!wanted.trim().is_empty()).then(|| wanted.trim().to_string());

                match api::profiles::upload_file(id, &file, name.as_deref()).await {
                    Ok(_) => {
                        error.set(None);
                        path.set(String::new());
                        reload.emit(());
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let body = match (&files.data, &files.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the files."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "No files. A profile with only preferences still delivers a package." }
            </p>
        },
        (Some(list), _) => html! {
            <ul class="profile-file-list">
                { for list.iter().map(|file| html! {
                    <li key={file.id}>
                        <FileRow
                            {id}
                            file={file.clone()}
                            on_changed={
                                let (reload, on_changed) =
                                    (files.reload.clone(), props.on_changed.clone());
                                Callback::from(move |_| {
                                    reload.emit(());
                                    on_changed.emit(());
                                })
                            }
                        />
                    </li>
                }) }
            </ul>
        },
    };

    html! {
        <Card
            title="Files"
            subtitle="Delivered beside the preferences, at the path each one names."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That file could not be added."
                    message={message.clone()}
                />
            }

            <Field
                label="Delivered path"
                id="profile-file-path"
                help="Where the device stores it. Leave it empty to use the file's own name."
            >
                <TextInput
                    id="profile-file-path"
                    value={(*path).clone()}
                    placeholder="maps/source.xml"
                    disabled={*busy}
                    onchange={
                        let path = path.clone();
                        Callback::from(move |value: String| path.set(value))
                    }
                />
            </Field>

            <FileDrop
                id="profile-file-upload"
                label="Drop a file here, or choose one"
                help="One at a time. A file with the same path replaces the one there."
                busy={*busy}
                {onfiles}
            />

            { body }
        </Card>
    }
}

#[derive(Properties, PartialEq)]
struct FileRowProps {
    id: ProfileId,
    file: ProfileFile,
    on_changed: Callback<()>,
}

#[function_component(FileRow)]
fn file_row(props: &FileRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let remove = {
        let (id, file) = (props.id, props.file.id);
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |_| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::profiles::delete_file(id, file).await {
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

    html! {
        <div class="profile-file-row">
            <div class="profile-file-row__identity">
                <span class="profile-file-row__name">{ props.file.name.clone() }</span>
                if let Some(mime) = &props.file.mime_type {
                    <span class="profile-file-row__mime">{ mime.clone() }</span>
                }
            </div>

            <div class="profile-file-row__meta">
                <span>{ size_label(props.file.size) }</span>
                <span>{ format!("Added {}", short_relative(props.file.updated)) }</span>
            </div>

            <ConfirmButton
                label="Remove"
                confirm_label="Remove it"
                question={format!(
                    "Remove '{}'? Devices that already have it keep it until they fetch the \
                     profile again.",
                    props.file.name,
                )}
                busy={*busy}
                onconfirm={remove}
            />

            if let Some(message) = &*error {
                <p class="profile-file-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_reported_in_the_roundest_unit_that_still_says_something() {
        assert_eq!(size_label(0), "0 B");
        assert_eq!(size_label(1023), "1023 B");
        assert_eq!(size_label(1024), "1.0 kB");
        assert_eq!(size_label(1_048_576), "1.0 MB");
    }
}
