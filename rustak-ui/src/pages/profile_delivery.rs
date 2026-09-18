//! The delivery card on a profile's page: when it is handed out, and to whom.
//!
//! Its own file because it is its own endpoint. `PATCH /profiles/{id}` takes
//! only what changed, so this card has a draft, a Save and a failure of its
//! own, separate from the preferences and files beside it — a single Save over
//! all three would either half-apply or have to be undone, and neither is
//! something a page can do honestly.

use rustak_api::{Profile, ProfileUpdate};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonGroup, ButtonKind, Card, ConfirmButton, Field, LoadingNote,
    Switch, TextInput,
};
use crate::util::nav_href;

use super::load::{use_download, use_resource};

#[derive(Properties, PartialEq)]
pub struct SettingsProps {
    pub profile: Profile,
    pub on_changed: Callback<()>,
}

#[function_component(Settings)]
pub fn settings(props: &SettingsProps) -> Html {
    let draft = use_state(|| props.profile.clone());
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // The loaded profile is the source of truth: a save, a refresh or another
    // administrator's change replaces the draft rather than being shouted down
    // by it.
    {
        let draft = draft.clone();
        let profile = props.profile.clone();
        use_effect_with(profile.clone(), move |_| {
            draft.set(profile);
            || ()
        });
    }

    let channels = use_resource(api::groups::list);

    let preview = {
        let id = props.profile.id;
        use_download(move || async move { api::profiles::preview(id).await })
    };

    let changed = *draft != props.profile;

    let save = {
        let (draft, original) = (draft.clone(), props.profile.clone());
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |_: MouseEvent| {
            let change = difference(&original, &draft);
            if change.is_empty() {
                return;
            }

            let id = original.id;
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());

            busy.set(true);
            spawn_local(async move {
                match api::profiles::patch(id, &change).await {
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
        let id = props.profile.id;
        let (busy, error) = (busy.clone(), error.clone());
        Callback::from(move |_| {
            let (busy, error) = (busy.clone(), error.clone());
            busy.set(true);
            spawn_local(async move {
                match api::profiles::remove(id).await {
                    // Deleting the thing this page is about leaves nothing to
                    // show, so the page the reader came from is where they go.
                    Ok(()) => crate::util::window()
                        .location()
                        .set_href(&nav_href("/admin/profiles"))
                        .unwrap_or_default(),
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let field = |part: &str| format!("profile-{part}");
    let edit = |change: fn(&mut Profile, String)| {
        let draft = draft.clone();
        Callback::from(move |value: String| {
            let mut next = (*draft).clone();
            change(&mut next, value);
            draft.set(next);
        })
    };
    let toggle = |change: fn(&mut Profile, bool)| {
        let draft = draft.clone();
        Callback::from(move |value: bool| {
            let mut next = (*draft).clone();
            change(&mut next, value);
            draft.set(next);
        })
    };

    html! {
        <Card
            title="Delivery"
            subtitle="When this profile is handed out, and to whom."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That change could not be saved."
                    message={message.clone()}
                />
            }
            if let Some(message) = &preview.error {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not build the preview."
                    message={message.clone()}
                />
            }

            <Field label="Description" id={field("description")}>
                <TextInput
                    id={field("description")}
                    value={draft.description.clone().unwrap_or_default()}
                    placeholder="What this profile is for"
                    disabled={*busy}
                    onchange={edit(|profile, value| profile.description = Some(value))}
                />
            </Field>

            <Field
                label="Tool"
                id={field("tool")}
                help="The name a client asks for this profile by, on /device/profile/tool/{tool}."
            >
                <TextInput
                    id={field("tool")}
                    value={draft.tool.clone().unwrap_or_default()}
                    placeholder="public"
                    disabled={*busy}
                    onchange={edit(|profile, value| profile.tool = Some(value))}
                />
            </Field>

            <Field label="Delivered" id={field("active")}>
                <Switch
                    id={field("active")}
                    checked={draft.active}
                    label="Active"
                    disabled={*busy}
                    onchange={toggle(|profile, value| profile.active = value)}
                />
                <Switch
                    id={field("on-enrollment")}
                    checked={draft.apply_on_enrollment}
                    label="On enrolment"
                    disabled={*busy}
                    onchange={toggle(|profile, value| profile.apply_on_enrollment = value)}
                />
                <Switch
                    id={field("on-connect")}
                    checked={draft.apply_on_connect}
                    label="On connect"
                    disabled={*busy}
                    onchange={toggle(|profile, value| profile.apply_on_connect = value)}
                />
            </Field>

            <Field
                label="Channels"
                id={field("channels")}
                help="Whose members receive it. None selected means everybody."
            >
                { channel_picker(&channels.data, &draft, *busy) }
            </Field>

            <ButtonGroup>
                <Button
                    kind={ButtonKind::Primary}
                    busy={*busy}
                    disabled={!changed}
                    title={(!changed).then_some("Nothing has changed.")}
                    onclick={save}
                >
                    { "Save" }
                </Button>

                <Button
                    busy={preview.busy}
                    title={Some(AttrValue::from(
                        "Download the package a device would receive, built by the server.",
                    ))}
                    onclick={
                        let start = preview.start.clone();
                        Callback::from(move |_: MouseEvent| start.emit(()))
                    }
                >
                    { "Preview package" }
                </Button>

                <ConfirmButton
                    label="Delete"
                    confirm_label="Delete it"
                    question={format!(
                        "Delete '{}'? Its preferences and files go with it.",
                        props.profile.name,
                    )}
                    busy={*busy}
                    onconfirm={remove}
                />
            </ButtonGroup>
        </Card>
    }
}

/// The channels this profile is limited to, as one switch each.
///
/// Not [`crate::components::GroupsPicker`]: that one edits a *membership*,
/// which has a direction and a source behind it. A profile's channel list is a
/// plain set of names, and offering Write/Read switches for it would be
/// offering a choice the endpoint has nowhere to put.
fn channel_picker(
    channels: &Option<Vec<rustak_api::Group>>,
    draft: &UseStateHandle<Profile>,
    busy: bool,
) -> Html {
    let Some(channels) = channels else {
        return html! { <LoadingNote label="Loading channels…" /> };
    };

    html! {
        <div class="channel-chips">
            { for channels.iter().map(|channel| {
                let name = channel.name.clone();
                let held = draft.groups.contains(&name);

                html! {
                    <Switch
                        key={channel.id.get()}
                        id={format!("profile-channel-{}", channel.bitpos)}
                        checked={held}
                        label={channel.name.to_string()}
                        disabled={busy}
                        onchange={
                            let (draft, name) = (draft.clone(), name.clone());
                            Callback::from(move |on: bool| {
                                let mut next = (*draft).clone();
                                next.groups.retain(|held| *held != name);
                                if on {
                                    next.groups.push(name.clone());
                                }
                                draft.set(next);
                            })
                        }
                    />
                }
            }) }
        </div>
    }
}

/// The change to send: only the fields that actually differ.
///
/// The server refuses an update that would do nothing, so sending the whole
/// row every time would turn "Save" with nothing changed into an error rather
/// than a no-op — and sending a field that has not changed would overwrite
/// whatever somebody else did to it in the meantime.
fn difference(original: &Profile, draft: &Profile) -> ProfileUpdate {
    let changed = |a: &Option<String>, b: &Option<String>| {
        let normalise = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        };

        (normalise(a) != normalise(b)).then(|| normalise(b).unwrap_or_default())
    };

    ProfileUpdate {
        description: changed(&original.description, &draft.description),
        tool: changed(&original.tool, &draft.tool),
        kind: None,
        active: (original.active != draft.active).then_some(draft.active),
        apply_on_enrollment: (original.apply_on_enrollment != draft.apply_on_enrollment)
            .then_some(draft.apply_on_enrollment),
        apply_on_connect: (original.apply_on_connect != draft.apply_on_connect)
            .then_some(draft.apply_on_connect),
        groups: (original.groups != draft.groups).then(|| draft.groups.clone()),
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::{GroupName, ProfileId};

    use super::*;

    fn profile() -> Profile {
        Profile {
            id: ProfileId::new(1),
            name: "Enrolment".to_string(),
            description: Some("Notes".to_string()),
            active: true,
            apply_on_enrollment: true,
            apply_on_connect: false,
            tool: None,
            kind: None,
            groups: Vec::new(),
            updated: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            file_count: 0,
            pref_count: 0,
        }
    }

    #[test]
    fn an_untouched_draft_produces_a_change_the_server_would_refuse_to_apply() {
        assert!(difference(&profile(), &profile()).is_empty());
    }

    #[test]
    fn only_what_moved_is_sent() {
        let mut draft = profile();
        draft.apply_on_connect = true;

        let change = difference(&profile(), &draft);

        assert_eq!(change.apply_on_connect, Some(true));
        assert_eq!(change.active, None, "what did not move is not sent");
        assert_eq!(change.description, None);
    }

    #[test]
    fn clearing_a_field_is_a_change_and_blank_is_the_same_as_absent() {
        let mut draft = profile();
        draft.description = Some("   ".to_string());

        assert_eq!(
            difference(&profile(), &draft).description.as_deref(),
            Some(""),
            "whitespace clears it",
        );

        let mut unchanged = profile();
        unchanged.tool = Some(String::new());
        assert_eq!(
            difference(&profile(), &unchanged).tool,
            None,
            "an empty string where there was nothing is not a change",
        );
    }

    #[test]
    fn emptying_the_channel_list_is_a_change_because_it_means_everybody() {
        let mut original = profile();
        original.groups = vec![GroupName::from_storage("Command")];

        let change = difference(&original, &profile());

        assert_eq!(change.groups, Some(Vec::new()));
        assert!(!change.is_empty());
    }
}
