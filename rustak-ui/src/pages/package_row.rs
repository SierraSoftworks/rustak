//! One package's row: what it is, who can reach it, and the three things an
//! operator changes about it.
//!
//! Its own file because the row is where every write on this page lives — a
//! `PATCH` may rename it, move it between channels and decide whether every
//! enrolling device gets a copy, and each of those needs its own draft and its
//! own failure.
//!
//! # A change reaches every row with the same hash
//!
//! A hash may back several rows — the same photograph attached to two map
//! items, the same package uploaded by two people — and the server writes all
//! of them. The confirmation for a delete says so, because "delete this
//! package" and "delete this file everywhere it appears" are different
//! promises.

use rustak_api::{Group, PackageSummary, PackageUpdate};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Button, ConfirmButton, MenuAction, MenuItem, SplitButton, StatusPill, StatusTone, Switch,
    TextInput,
};
use crate::util::short_relative;

use super::load::{use_download, use_resource};

/// How a size is worth reading on a row.
pub fn size_label(bytes: i64) -> String {
    match bytes.max(0) {
        bytes @ 0..=1023 => format!("{bytes} B"),
        bytes @ 1024..=1_048_575 => format!("{:.1} kB", bytes as f64 / 1024.0),
        bytes => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

#[derive(Properties, PartialEq)]
pub struct PackageRowProps {
    pub package: PackageSummary,
    pub on_changed: Callback<()>,
}

#[function_component(PackageRow)]
pub fn package_row(props: &PackageRowProps) -> Html {
    let package = &props.package;
    let channels = use_resource(api::groups::list);

    let name = use_state(|| package.name.clone());
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let open = use_state(|| false);

    {
        let name = name.clone();
        let stored = package.name.clone();
        use_effect_with(stored.clone(), move |_| {
            name.set(stored);
            || ()
        });
    }

    let download = {
        let package = package.clone();
        use_download(move || {
            let package = package.clone();
            async move { api::packages::content(&package).await }
        })
    };

    let apply = {
        let hash = package.hash.clone();
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |change: PackageUpdate| {
            if change.is_empty() {
                return;
            }

            let hash = hash.clone();
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());

            busy.set(true);
            spawn_local(async move {
                match api::packages::patch(&hash, &change).await {
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

    let remove = {
        let hash = package.hash.clone();
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |_| {
            let hash = hash.clone();
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            spawn_local(async move {
                match api::packages::remove(&hash).await {
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

    let renamed = name.trim() != package.name && !name.trim().is_empty();

    html! {
        <div class="package-row">
            <div class="package-row__identity">
                <span class="package-row__name">{ package.name.clone() }</span>
                if let Some(filename) = &package.filename {
                    <span class="package-row__filename">{ filename.clone() }</span>
                }
            </div>

            <div class="package-row__meta">
                <span>{ size_label(package.size) }</span>
                <span>{ package.mime_type.clone() }</span>
                if let Some(submitter) = &package.submitter {
                    <span title="Who uploaded it">{ submitter.clone() }</span>
                }
                <span>{ format!("Added {}", short_relative(package.submission_time)) }</span>
                <span title="Whose members may fetch it">
                    { match package.groups.is_empty() {
                        true => "No channel".to_string(),
                        false => package.groups.join(", "),
                    } }
                </span>
                if let Some(mission) = &package.mission_name {
                    <span title="The mission it is filed under">{ mission.clone() }</span>
                }
                if !package.keywords.is_empty() {
                    <span>{ package.keywords.join(" · ") }</span>
                }
            </div>

            <div class="package-row__state">
                if package.mission_package {
                    <StatusPill
                        tone={StatusTone::Ok}
                        label="Mission package"
                        title="Listed in a client's own data-package browser."
                    />
                }
                if package.install_on_enrollment {
                    <StatusPill
                        tone={StatusTone::Warning}
                        label="On enrolment"
                        title="Handed to every device that enrols."
                    />
                }
            </div>

            // Fetching the bytes is what a row is usually there for; the
            // editor, which changes what the row says, opens from the menu.
            <SplitButton
                busy={download.busy}
                menu_label={format!("More actions for {}", package.name)}
                primary={MenuAction::new("Download", download.start.clone())}
                items={vec![MenuItem::Action(MenuAction::new(
                    if *open { "Close" } else { "Edit" },
                    {
                        let open = open.clone();
                        Callback::from(move |()| open.set(!*open))
                    },
                ))]}
            />

            if let Some(message) = &download.error {
                <p class="package-row__error" role="alert">{ message.clone() }</p>
            }
            if let Some(message) = &*error {
                <p class="package-row__error" role="alert">{ message.clone() }</p>
            }

            if *open {
                <div class="package-row__editor">
                    <TextInput
                        id={format!("package-name-{}", package.hash)}
                        value={(*name).clone()}
                        placeholder="What operators and clients see"
                        disabled={*busy}
                        onchange={
                            let name = name.clone();
                            Callback::from(move |value: String| name.set(value))
                        }
                    />

                    <Button
                        small=true
                        busy={*busy}
                        disabled={!renamed}
                        title={(!renamed).then_some("Nothing has changed.")}
                        onclick={
                            let (apply, name) = (apply.clone(), name.clone());
                            Callback::from(move |_: MouseEvent| {
                                apply.emit(PackageUpdate {
                                    name: Some(name.trim().to_string()),
                                    ..PackageUpdate::default()
                                });
                            })
                        }
                    >
                        { "Rename" }
                    </Button>

                    <Switch
                        id={format!("package-enrol-{}", package.hash)}
                        checked={package.install_on_enrollment}
                        label="Install on enrolment"
                        disabled={*busy}
                        onchange={
                            let apply = apply.clone();
                            Callback::from(move |on: bool| {
                                apply.emit(PackageUpdate {
                                    install_on_enrollment: Some(on),
                                    ..PackageUpdate::default()
                                });
                            })
                        }
                    />

                    { channel_chips(&channels.data, package, *busy, &apply) }

                    <ConfirmButton
                        label="Delete"
                        confirm_label="Delete it"
                        question={format!(
                            "Delete '{}'? Every row holding these bytes goes, and a device that \
                             has not fetched it yet never will.",
                            package.name,
                        )}
                        busy={*busy}
                        onconfirm={remove}
                    />
                </div>
            }
        </div>
    }
}

/// The channels this package is in, as one switch each.
///
/// Each toggle is its own `PATCH` carrying the whole set, because that is what
/// `PackageUpdate::groups` is — a replacement rather than a delta, so dropping
/// one of the others would silently revoke it.
fn channel_chips(
    channels: &Option<Vec<Group>>,
    package: &PackageSummary,
    busy: bool,
    apply: &Callback<PackageUpdate>,
) -> Html {
    let Some(channels) = channels else {
        return html! {};
    };

    html! {
        <div class="channel-chips">
            { for channels.iter().map(|channel| {
                let name = channel.name.to_string();
                let held = package.groups.contains(&name);
                let current = package.groups.clone();

                html! {
                    <Switch
                        key={channel.id.get()}
                        id={format!("package-channel-{}-{}", package.hash, channel.bitpos)}
                        checked={held}
                        label={name.clone()}
                        disabled={busy}
                        onchange={
                            let apply = apply.clone();
                            Callback::from(move |on: bool| {
                                let mut wanted: Vec<String> = current
                                    .iter()
                                    .filter(|held| *held != &name)
                                    .cloned()
                                    .collect();
                                if on {
                                    wanted.push(name.clone());
                                }

                                apply.emit(PackageUpdate {
                                    groups: Some(wanted),
                                    ..PackageUpdate::default()
                                });
                            })
                        }
                    />
                }
            }) }
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
        assert_eq!(size_label(4_194_304), "4.0 MB");
    }

    #[test]
    fn a_size_the_server_could_not_measure_does_not_render_as_nonsense() {
        assert_eq!(size_label(-1), "0 B");
    }
}
