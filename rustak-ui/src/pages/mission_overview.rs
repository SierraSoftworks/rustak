//! A mission's Overview tab: what it is, and what happens to it.
//!
//! Split out of `mission_detail.rs` because it carries the two actions the
//! page exists for — the archive download and the delete — and each has its own
//! in-flight state and its own failure.
//!
//! The delete qualifier and the actions sit in a `stacked-actions` column: a
//! switch and a button group are both `inline-flex`, so left to themselves they
//! share a line and the long label runs under the buttons it qualifies.
//!
//! # Deleting is not immediate forgetting
//!
//! A deleted mission keeps its row so a client syncing late is told it went
//! (`410 Gone`) rather than simply failing to find it. `deep` additionally
//! removes the content that was only ever attached to it; without it the files
//! stay in the enterprise sync store, where anything else referencing them
//! still finds them.

use rustak_api::MissionDetail;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Button, ButtonGroup, Card, ConfirmButton, RoleBadge, StatusPill, StatusTone,
    Switch,
};
use crate::util::{format_iso8601, short_relative};

use super::load::use_download;

#[derive(Properties, PartialEq)]
pub struct OverviewProps {
    pub mission: MissionDetail,
}

#[function_component(Overview)]
pub fn overview(props: &OverviewProps) -> Html {
    let summary = &props.mission.summary;
    let guid = summary.guid;

    let deep = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // Deleting is the one action on this page that cannot be followed by a
    // re-read: `GET /missions/{guid}` answers `410 Gone` for a mission that has
    // been deleted, so reloading would replace a page that knows what happened
    // with one that could not say. The outcome of the call is therefore what is
    // shown, and the listing — which does carry deleted missions — is a click
    // away.
    let deleted = use_state(|| props.mission.summary.deleted_at);

    let archive = use_download(move || async move { api::missions::archive(&guid).await });

    let remove = {
        let deep = deep.clone();
        let (busy, error, deleted) = (busy.clone(), error.clone(), deleted.clone());
        Callback::from(move |_| {
            let deep = *deep;
            let (busy, error, deleted) = (busy.clone(), error.clone(), deleted.clone());
            busy.set(true);
            spawn_local(async move {
                match api::missions::remove(&guid, deep).await {
                    Ok(()) => {
                        error.set(None);
                        deleted.set(Some(chrono::Utc::now()));
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let deleted = *deleted;

    html! {
        <Card title="Overview" subtitle="What this mission is, and what happens to it.">
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That mission could not be deleted."
                    message={message.clone()}
                />
            }
            if let Some(message) = &archive.error {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not build the archive."
                    message={message.clone()}
                />
            }
            if let Some(at) = deleted {
                <Alert
                    kind={AlertKind::Warning}
                    title="This mission has been deleted."
                    message={format!(
                        "Deleted {}. Its row is kept so that a client syncing late is told it \
                         went rather than simply failing to find it — but the listing shows \
                         only live missions, so it will not be there.",
                        short_relative(at),
                    )}
                />
            }

            <dl class="detail-list">
                <dt>{ "Description" }</dt>
                <dd>{ summary.description.clone().unwrap_or_else(|| "—".to_string()) }</dd>

                <dt>{ "Created" }</dt>
                <dd>{ format_iso8601(summary.create_time) }</dd>

                <dt>{ "Created by" }</dt>
                <dd>
                    <code>
                        { summary.creator_uid.clone().unwrap_or_else(|| "—".to_string()) }
                    </code>
                </dd>

                <dt>{ "Channels" }</dt>
                <dd>
                    { match summary.groups.is_empty() {
                        true => "No channel — only an administrator sees it.".to_string(),
                        false => summary.groups
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    } }
                </dd>

                <dt>{ "Keywords" }</dt>
                <dd>
                    { match summary.keywords.is_empty() {
                        true => "—".to_string(),
                        false => summary.keywords.join(", "),
                    } }
                </dd>

                <dt>{ "Expires" }</dt>
                <dd>
                    { summary.expiration.map(format_iso8601)
                        .unwrap_or_else(|| "Never".to_string()) }
                </dd>

                <dt>{ "Contents" }</dt>
                <dd>
                    { format!(
                        "{} items · {} files · {} changes",
                        summary.uid_count, summary.content_count, props.mission.change_count,
                    ) }
                </dd>

                <dt>{ "New subscribers get" }</dt>
                <dd><RoleBadge role={summary.default_role} /></dd>

                <dt>{ "Joining" }</dt>
                <dd class="detail-list__pills">
                    <StatusPill
                        tone={if summary.password_protected {
                            StatusTone::Warning
                        } else {
                            StatusTone::Neutral
                        }}
                        label={if summary.password_protected {
                            "Password required"
                        } else {
                            "No password"
                        }}
                    />
                    <StatusPill
                        tone={StatusTone::Neutral}
                        label={if summary.invite_only { "Invite only" } else { "Open" }}
                        title={summary.invite_only.then_some(
                            "Hidden from a client's own listing until it is invited.",
                        )}
                    />
                </dd>
            </dl>

            <div class="stacked-actions">
                <Switch
                    id="mission-delete-deep"
                    checked={*deep}
                    label="Also delete content attached only to this mission"
                    disabled={*busy || deleted.is_some()}
                    onchange={
                        let deep = deep.clone();
                        Callback::from(move |value: bool| deep.set(value))
                    }
                />

                <ButtonGroup>
                    <Button
                        busy={archive.busy}
                        title={Some(AttrValue::from(
                            "Download everything in this mission as a Mission Package.",
                        ))}
                        onclick={
                            let start = archive.start.clone();
                            Callback::from(move |_: MouseEvent| start.emit(()))
                        }
                    >
                        { "Download archive" }
                    </Button>

                    <ConfirmButton
                        label="Delete"
                        confirm_label="Delete it"
                        question={format!(
                            "Delete '{}'? Every subscriber loses it{}.",
                            summary.name,
                            if *deep { ", and its attached content goes too" } else { "" },
                        )}
                        busy={*busy}
                        disabled={deleted.is_some()}
                        title={deleted.is_some().then_some("It has already been deleted.")}
                        onconfirm={remove}
                    />
                </ButtonGroup>
            </div>
        </Card>
    }
}
