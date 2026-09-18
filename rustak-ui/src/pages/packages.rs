//! The data packages this server hands out.
//!
//! Reading this list is not administrative — it follows the same visibility
//! rule `/Marti/sync/search` does, so what an operator sees here is what a
//! client browsing its own channels would see. Everything that *changes* a
//! package is, because a package is delivered to devices and
//! `install on enrolment` puts one on every device that enrols.
//!
//! That flag is the reason this page has an editor at all: it is the one
//! setting on a package that changes what happens to a device nobody has
//! touched yet.

use rustak_api::PackageSummary;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::api::packages::PackageFilter;
use crate::components::{Alert, AlertKind, Card, Field, FileDrop, LoadingNote, Switch, TextInput};

use super::load::{use_refresh_action, use_resource};
use super::package_row::PackageRow;

#[function_component(Packages)]
pub fn packages() -> Html {
    let filter = use_state(PackageFilter::default);
    let wanted = (*filter).clone();

    let packages = use_resource(move || {
        let wanted = wanted.clone();
        async move { api::packages::list(&wanted).await }
    });
    use_refresh_action(packages.reload.clone(), packages.busy);

    // `use_resource` fetches on mount and on reload, so a change to the filter
    // has to ask for a fetch rather than merely changing what the next one
    // would send.
    {
        let reload = packages.reload.clone();
        use_effect_with((*filter).clone(), move |_| {
            reload.emit(());
            || ()
        });
    }

    let body = match (&packages.data, &packages.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the packages."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "Nothing matches. A client uploads a data package from its own map, and an \
                   operator can drop one in above." }
            </p>
        },
        (Some(list), _) => html! {
            <ul class="package-list">
                { for list.iter().map(|package| html! {
                    <li key={package.hash.clone()}>
                        <PackageRow
                            package={package.clone()}
                            on_changed={packages.reload.clone()}
                        />
                    </li>
                }) }
            </ul>
        },
    };

    html! {
        <>
            <Upload on_created={packages.reload.clone()} />

            <Card
                title="All packages"
                subtitle="What this server holds, and who may fetch each one."
            >
                <Field
                    label="Filter"
                    id="package-filter"
                    help="Matches a name or a keyword, ignoring case."
                >
                    <TextInput
                        id="package-filter"
                        value={filter.query.clone()}
                        placeholder="patrol"
                        onchange={
                            let filter = filter.clone();
                            Callback::from(move |value: String| {
                                filter.set(PackageFilter { query: value, ..(*filter).clone() });
                            })
                        }
                    />
                </Field>

                <Switch
                    id="package-mission-only"
                    checked={filter.mission_package}
                    label="Mission packages only — what a client's data-package browser lists"
                    onchange={
                        let filter = filter.clone();
                        Callback::from(move |on: bool| {
                            filter.set(PackageFilter { mission_package: on, ..(*filter).clone() });
                        })
                    }
                />

                { body }
            </Card>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct UploadProps {
    on_created: Callback<()>,
}

#[function_component(Upload)]
fn upload(props: &UploadProps) -> Html {
    let name = use_state(String::new);
    let keywords = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let uploaded = use_state(|| None::<PackageSummary>);

    let onfiles = {
        let (name, keywords) = (name.clone(), keywords.clone());
        let (busy, error, uploaded) = (busy.clone(), error.clone(), uploaded.clone());
        let on_created = props.on_created.clone();

        Callback::from(move |chosen: Vec<web_sys::File>| {
            let Some(file) = chosen.into_iter().next() else {
                return;
            };

            let wanted = (*name).clone();
            let words: Vec<String> = keywords
                .split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(ToString::to_string)
                .collect();

            let (name, keywords) = (name.clone(), keywords.clone());
            let (busy, error, uploaded) = (busy.clone(), error.clone(), uploaded.clone());
            let on_created = on_created.clone();

            busy.set(true);
            spawn_local(async move {
                let chosen = (!wanted.trim().is_empty()).then(|| wanted.trim().to_string());

                match api::packages::create(&file, chosen.as_deref(), None, &words, &[]).await {
                    Ok(package) => {
                        error.set(None);
                        uploaded.set(Some(package));
                        name.set(String::new());
                        keywords.set(String::new());
                        on_created.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    html! {
        <Card
            title="Add a package"
            subtitle="Channels and the enrolment flag are set on the row once it is here."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That package could not be uploaded."
                    message={message.clone()}
                />
            } else if let Some(package) = &*uploaded {
                <Alert
                    kind={AlertKind::Success}
                    title={format!("'{}' was stored.", package.name)}
                    message="An upload that names no channel lands in the one everybody holds. \
                             Narrow it on the row below if it is not for everybody."
                />
            }

            <div class="inline-form">
                <Field
                    label="Name"
                    id="package-name"
                    help="What operators and clients see. Defaults to the filename."
                >
                    <TextInput
                        id="package-name"
                        value={(*name).clone()}
                        placeholder="Patrol brief"
                        disabled={*busy}
                        onchange={
                            let name = name.clone();
                            Callback::from(move |value: String| name.set(value))
                        }
                    />
                </Field>

                <Field
                    label="Keywords"
                    id="package-keywords"
                    help="Comma separated. 'missionpackage' is what puts it in a client's \
                          data-package browser."
                >
                    <TextInput
                        id="package-keywords"
                        value={(*keywords).clone()}
                        placeholder="missionpackage, brief"
                        disabled={*busy}
                        onchange={
                            let keywords = keywords.clone();
                            Callback::from(move |value: String| keywords.set(value))
                        }
                    />
                </Field>
            </div>

            <FileDrop
                id="package-upload"
                label="Drop a package here, or choose one"
                help="One at a time. The bytes go straight from the browser to the store."
                busy={*busy}
                {onfiles}
            />
        </Card>
    }
}
