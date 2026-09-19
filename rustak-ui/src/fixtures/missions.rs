//! Demo data and behaviour for Data Sync missions.
//!
//! Three missions that between them exercise every branch the pages have: one
//! ordinary, one password-protected and invite-only with a read-only
//! subscriber, and one that has already been deleted — because `410 Gone` is a
//! state the detail page has to render and a listing that never contains one
//! would never show it.

use std::cell::RefCell;

use rustak_api::{
    GroupName, MissionChangeKind, MissionChangeSummary, MissionDetail, MissionGuid,
    MissionLayerSummary, MissionRoleKind, MissionSubscriptionSummary, MissionSummary, UidDetails,
};

use super::data::ago;
use super::empty_zip;
use crate::api::ApiError;
use crate::api::download::Download;

/// The guids the fixtures use, spelled once so a deep link into demo mode can
/// be written down.
const KETTLE: &str = "0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f";
const RESCUE: &str = "6b2c8f41-77aa-4d19-b0c5-2e9f13d8a704";

/// The mission that has already been deleted.
///
/// Public because it is the one fixture whose *identifier* is worth writing
/// down: `/admin/missions/f0a19c52-…?demo` is how the deleted-mission detail
/// is looked at without a server, and that is the one view of this console
/// that cannot be reached by clicking — the listing does not carry deleted
/// missions.
pub const STANDDOWN: &str = "f0a19c52-3e64-4b8d-8a17-55c2d9e40b31";

struct State {
    missions: Vec<MissionSummary>,
    subscriptions: Vec<(MissionGuid, Vec<MissionSubscriptionSummary>)>,
    layers: Vec<(MissionGuid, Vec<MissionLayerSummary>)>,
    changes: Vec<(MissionGuid, Vec<MissionChangeSummary>)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

fn with<R>(action: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| action(&mut state.borrow_mut()))
}

fn guid(value: &str) -> MissionGuid {
    MissionGuid::parse(value).expect("the fixture guids are valid UUIDs")
}

fn missing() -> ApiError {
    ApiError::Server("There is no mission with that identifier.".to_string())
}

fn subscription(
    client_uid: &str,
    username: Option<&str>,
    role: MissionRoleKind,
    minutes: i64,
    connected: bool,
) -> MissionSubscriptionSummary {
    MissionSubscriptionSummary {
        client_uid: client_uid.to_string(),
        username: username.map(ToString::to_string),
        role,
        create_time: ago(minutes),
        connected,
    }
}

fn change(
    kind: MissionChangeKind,
    uid: &str,
    minutes: i64,
    creator: &str,
    details: Option<UidDetails>,
) -> MissionChangeSummary {
    MissionChangeSummary {
        kind,
        content_uid: Some(uid.to_string()),
        content_hash: None,
        timestamp: ago(minutes),
        server_time: ago(minutes),
        creator_uid: Some(creator.to_string()),
        details,
    }
}

fn marker(callsign: &str, kind: &str) -> UidDetails {
    UidDetails {
        kind: Some(kind.to_string()),
        callsign: Some(callsign.to_string()),
        lat: Some(51.507),
        lon: Some(-0.127),
        ..UidDetails::default()
    }
}

impl State {
    fn new() -> Self {
        let kettle = guid(KETTLE);
        let rescue = guid(RESCUE);
        let standdown = guid(STANDDOWN);

        Self {
            missions: vec![
                MissionSummary {
                    guid: kettle,
                    name: "Operation Kettle".to_string(),
                    description: Some("The current picture for the Command channel.".to_string()),
                    tool: "public".to_string(),
                    creator_uid: Some("ANDROID-2f1c9a7b4e0d".to_string()),
                    create_time: ago(60 * 24 * 3),
                    groups: vec![GroupName::from_storage("Command")],
                    keywords: vec!["recon".to_string(), "live".to_string()],
                    subscriber_count: 3,
                    uid_count: 6,
                    content_count: 2,
                    password_protected: false,
                    invite_only: false,
                    default_role: MissionRoleKind::Subscriber,
                    expiration: None,
                    deleted_at: None,
                },
                MissionSummary {
                    guid: rescue,
                    name: "Rescue 12".to_string(),
                    description: None,
                    tool: "public".to_string(),
                    creator_uid: Some("IOS-91ac4d55f207".to_string()),
                    create_time: ago(60 * 7),
                    groups: vec![GroupName::from_storage("Blue Team")],
                    keywords: Vec::new(),
                    subscriber_count: 2,
                    uid_count: 2,
                    content_count: 0,
                    password_protected: true,
                    invite_only: true,
                    default_role: MissionRoleKind::ReadonlySubscriber,
                    expiration: Some(ago(-60 * 24 * 2)),
                    deleted_at: None,
                },
                MissionSummary {
                    guid: standdown,
                    name: "Stand-down".to_string(),
                    description: Some("Kept so a client syncing late is told it went.".to_string()),
                    tool: "public".to_string(),
                    creator_uid: Some("WINTAK-7b3e10cc".to_string()),
                    create_time: ago(60 * 24 * 30),
                    groups: Vec::new(),
                    keywords: Vec::new(),
                    subscriber_count: 0,
                    uid_count: 0,
                    content_count: 0,
                    password_protected: false,
                    invite_only: false,
                    default_role: MissionRoleKind::Subscriber,
                    expiration: None,
                    deleted_at: Some(ago(60 * 24 * 2)),
                },
            ],
            subscriptions: vec![
                (
                    kettle,
                    vec![
                        subscription(
                            "ANDROID-2f1c9a7b4e0d",
                            Some("avery"),
                            MissionRoleKind::Owner,
                            60 * 24 * 3,
                            true,
                        ),
                        subscription(
                            "IOS-91ac4d55f207",
                            Some("bhavna"),
                            MissionRoleKind::Subscriber,
                            60 * 20,
                            true,
                        ),
                        subscription(
                            "WINTAK-7b3e10cc",
                            Some("avery"),
                            MissionRoleKind::ReadonlySubscriber,
                            60 * 9,
                            false,
                        ),
                    ],
                ),
                (
                    rescue,
                    vec![
                        subscription(
                            "IOS-91ac4d55f207",
                            Some("bhavna"),
                            MissionRoleKind::Owner,
                            60 * 7,
                            true,
                        ),
                        subscription(
                            "ATAK-unknown-4419",
                            None,
                            MissionRoleKind::Subscriber,
                            90,
                            false,
                        ),
                    ],
                ),
                (standdown, Vec::new()),
            ],
            layers: vec![
                (
                    kettle,
                    vec![
                        MissionLayerSummary {
                            uid: "layer-markers".to_string(),
                            name: Some("Markers".to_string()),
                            kind: "UID".to_string(),
                            parent_uid: None,
                            position: 0,
                            item_count: 4,
                        },
                        MissionLayerSummary {
                            uid: "layer-markers-north".to_string(),
                            name: Some("North sector".to_string()),
                            kind: "UID".to_string(),
                            parent_uid: Some("layer-markers".to_string()),
                            position: 0,
                            item_count: 2,
                        },
                        MissionLayerSummary {
                            uid: "layer-files".to_string(),
                            name: Some("Attachments".to_string()),
                            kind: "CONTENTS".to_string(),
                            parent_uid: None,
                            position: 1,
                            item_count: 2,
                        },
                    ],
                ),
                (rescue, Vec::new()),
                (standdown, Vec::new()),
            ],
            changes: vec![
                (
                    kettle,
                    vec![
                        change(
                            MissionChangeKind::CreateMission,
                            "MISSION",
                            60 * 24 * 3,
                            "ANDROID-2f1c9a7b4e0d",
                            None,
                        ),
                        change(
                            MissionChangeKind::AddContent,
                            "UID-ALPHA",
                            60 * 22,
                            "ANDROID-2f1c9a7b4e0d",
                            Some(marker("ALPHA", "a-f-G-U-C")),
                        ),
                        change(
                            MissionChangeKind::AddContent,
                            "UID-BRAVO",
                            60 * 18,
                            "IOS-91ac4d55f207",
                            Some(marker("BRAVO", "a-h-G")),
                        ),
                        change(
                            MissionChangeKind::RemoveContent,
                            "UID-ALPHA",
                            60 * 4,
                            "IOS-91ac4d55f207",
                            Some(marker("ALPHA", "a-f-G-U-C")),
                        ),
                        change(
                            MissionChangeKind::AddContent,
                            "UID-ALPHA",
                            60,
                            "ANDROID-2f1c9a7b4e0d",
                            Some(marker("ALPHA", "a-f-G-U-C")),
                        ),
                    ],
                ),
                (
                    rescue,
                    vec![change(
                        MissionChangeKind::CreateMission,
                        "MISSION",
                        60 * 7,
                        "IOS-91ac4d55f207",
                        None,
                    )],
                ),
                (
                    standdown,
                    vec![change(
                        MissionChangeKind::DeleteMission,
                        "MISSION",
                        60 * 24 * 2,
                        "WINTAK-7b3e10cc",
                        None,
                    )],
                ),
            ],
        }
    }
}

/// Squashes a change log the way the server does: one entry per piece of
/// content, carrying whether it is still there.
///
/// Written here rather than reused from the server because `rustak-ui` cannot
/// depend on `rustak-server`; it is deliberately the naive version, which is
/// exactly what the server's own property test checks its optimised one
/// against.
fn squash(changes: &[MissionChangeSummary]) -> Vec<MissionChangeSummary> {
    let mut latest: Vec<MissionChangeSummary> = Vec::new();

    for entry in changes {
        match latest
            .iter_mut()
            .find(|held| held.content_uid == entry.content_uid)
        {
            Some(held) => *held = entry.clone(),
            None => latest.push(entry.clone()),
        }
    }

    latest
        .into_iter()
        .filter(|entry| entry.kind != MissionChangeKind::RemoveContent)
        .collect()
}

pub fn missions() -> Vec<MissionSummary> {
    with(|state| state.missions.clone())
}

pub fn mission(wanted: &MissionGuid) -> Result<MissionDetail, ApiError> {
    with(|state| {
        let summary = state
            .missions
            .iter()
            .find(|mission| mission.guid == *wanted)
            .cloned()
            .ok_or_else(missing)?;

        let find = |held: &MissionGuid| *held == *wanted;

        Ok(MissionDetail {
            change_count: state
                .changes
                .iter()
                .find(|(held, _)| find(held))
                .map(|(_, entries)| entries.len() as u32)
                .unwrap_or_default(),
            summary,
            subscriptions: state
                .subscriptions
                .iter()
                .find(|(held, _)| find(held))
                .map(|(_, entries)| entries.clone())
                .unwrap_or_default(),
            layers: state
                .layers
                .iter()
                .find(|(held, _)| find(held))
                .map(|(_, entries)| entries.clone())
                .unwrap_or_default(),
        })
    })
}

pub fn mission_changes(
    wanted: &MissionGuid,
    squashed: bool,
) -> Result<Vec<MissionChangeSummary>, ApiError> {
    with(|state| {
        let entries = state
            .changes
            .iter()
            .find(|(held, _)| *held == *wanted)
            .map(|(_, entries)| entries.clone())
            .ok_or_else(missing)?;

        Ok(match squashed {
            true => squash(&entries),
            false => entries,
        })
    })
}

pub fn set_mission_role(
    wanted: &MissionGuid,
    client_uid: &str,
    role: MissionRoleKind,
) -> Result<(), ApiError> {
    with(|state| {
        let held = state
            .subscriptions
            .iter_mut()
            .find(|(held, _)| *held == *wanted)
            .ok_or_else(missing)?;

        let found = held
            .1
            .iter_mut()
            .find(|subscription| subscription.client_uid == client_uid)
            .ok_or_else(|| ApiError::Server("That device is not subscribed.".to_string()))?;

        found.role = role;

        Ok(())
    })
}

pub fn unsubscribe_mission(wanted: &MissionGuid, client_uid: &str) -> Result<(), ApiError> {
    with(|state| {
        let held = state
            .subscriptions
            .iter_mut()
            .find(|(held, _)| *held == *wanted)
            .ok_or_else(missing)?;

        let before = held.1.len();
        held.1
            .retain(|subscription| subscription.client_uid != client_uid);
        let removed = held.1.len() < before;

        if removed {
            if let Some(mission) = state
                .missions
                .iter_mut()
                .find(|mission| mission.guid == *wanted)
            {
                mission.subscriber_count = mission.subscriber_count.saturating_sub(1);
            }

            return Ok(());
        }

        Err(ApiError::Server(
            "That device is not subscribed.".to_string(),
        ))
    })
}

pub fn delete_mission(wanted: &MissionGuid) -> Result<(), ApiError> {
    with(|state| {
        let mission = state
            .missions
            .iter_mut()
            .find(|mission| mission.guid == *wanted)
            .ok_or_else(missing)?;

        if mission.deleted_at.is_some() {
            return Err(ApiError::Gone);
        }

        mission.deleted_at = Some(chrono::Utc::now());
        mission.subscriber_count = 0;

        if let Some(held) = state
            .subscriptions
            .iter_mut()
            .find(|(held, _)| *held == *wanted)
        {
            held.1.clear();
        }

        Ok(())
    })
}

/// The mission archive. Demo mode has no archive builder behind it, so this is
/// a valid but empty zip — see the note on the profile preview.
pub fn mission_archive(wanted: &MissionGuid) -> Result<Download, ApiError> {
    let detail = mission(wanted)?;

    Ok(Download {
        filename: format!("{}.zip", detail.summary.name.replace(' ', "_")),
        mime: "application/zip".to_string(),
        bytes: empty_zip(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squashing_keeps_the_last_word_on_each_piece_of_content() {
        let entries = vec![
            change(MissionChangeKind::AddContent, "A", 30, "X", None),
            change(MissionChangeKind::AddContent, "B", 20, "X", None),
            change(MissionChangeKind::RemoveContent, "A", 10, "X", None),
        ];

        let squashed = squash(&entries);

        assert_eq!(squashed.len(), 1, "the removed one is gone: {squashed:?}");
        assert_eq!(squashed[0].content_uid.as_deref(), Some("B"));
    }

    #[test]
    fn content_added_back_after_a_removal_survives_the_squash() {
        let entries = vec![
            change(MissionChangeKind::AddContent, "A", 30, "X", None),
            change(MissionChangeKind::RemoveContent, "A", 20, "X", None),
            change(MissionChangeKind::AddContent, "A", 10, "X", None),
        ];

        assert_eq!(squash(&entries).len(), 1, "it is there again");
    }
}
