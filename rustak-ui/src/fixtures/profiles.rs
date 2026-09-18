//! Demo data and behaviour for device profiles.
//!
//! Its own store rather than a chapter of [`super::store`], because nothing
//! here is reachable from anything there: a profile has no user, no device and
//! no credential in it, so the two would only ever share a `RefCell`.
//!
//! It behaves like the server rather than like a screenshot — creating,
//! renaming, editing preferences and attaching files all stick for as long as
//! the tab lives, because the pages that exist to do those things cannot be
//! reviewed otherwise.

use std::cell::RefCell;

use rustak_api::{
    GroupName, PrefCatalogEntry, PrefClass, PrefEntry, Profile, ProfileCreate, ProfileFile,
    ProfileId, ProfileUpdate,
};

use super::data::ago;
use super::empty_zip;
use crate::api::ApiError;
use crate::api::download::Download;

struct State {
    profiles: Vec<Profile>,
    prefs: Vec<(ProfileId, Vec<PrefEntry>)>,
    files: Vec<(ProfileId, Vec<ProfileFile>)>,
    next_id: i64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

fn with<R>(action: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| action(&mut state.borrow_mut()))
}

fn group(name: &str) -> GroupName {
    GroupName::from_storage(name)
}

fn missing() -> ApiError {
    ApiError::Server("There is no profile with that identifier.".to_string())
}

impl State {
    fn new() -> Self {
        let enrolment = ProfileId::new(1);
        let command = ProfileId::new(2);
        let maps = ProfileId::new(3);

        Self {
            profiles: vec![
                Profile {
                    id: enrolment,
                    name: "Enrolment defaults".to_string(),
                    description: Some(
                        "What every device is handed the first time it enrols.".to_string(),
                    ),
                    active: true,
                    apply_on_enrollment: true,
                    apply_on_connect: false,
                    tool: None,
                    kind: None,
                    groups: Vec::new(),
                    updated: ago(60 * 24 * 9),
                    file_count: 0,
                    pref_count: 4,
                },
                Profile {
                    id: command,
                    name: "Command".to_string(),
                    description: Some("Extra map layers for the Command channel.".to_string()),
                    active: true,
                    apply_on_enrollment: false,
                    apply_on_connect: true,
                    tool: Some("public".to_string()),
                    kind: None,
                    groups: vec![group("Command")],
                    updated: ago(60 * 5),
                    file_count: 1,
                    pref_count: 2,
                },
                Profile {
                    id: maps,
                    name: "Offline maps".to_string(),
                    description: None,
                    active: false,
                    apply_on_enrollment: false,
                    apply_on_connect: true,
                    tool: Some("offline".to_string()),
                    kind: None,
                    groups: vec![group("Blue Team")],
                    updated: ago(60 * 24 * 40),
                    file_count: 2,
                    pref_count: 0,
                },
            ],
            prefs: vec![
                (
                    enrolment,
                    vec![
                        PrefEntry::string("deviceProfileEnableOnConnect", "true"),
                        PrefEntry::string("displayServerConnectionWidget", "true"),
                        PrefEntry::string("prefs_enable_channels", "true"),
                        PrefEntry::new("locationTeam", PrefClass::String, "Cyan"),
                    ],
                ),
                (
                    command,
                    vec![
                        PrefEntry::new("atakRoleType", PrefClass::String, "Team Lead"),
                        PrefEntry::new("coord_display_pref", PrefClass::String, "MGRS"),
                    ],
                ),
                (maps, Vec::new()),
            ],
            files: vec![
                (enrolment, Vec::new()),
                (
                    command,
                    vec![ProfileFile {
                        id: 41,
                        name: "maps/command-overlay.xml".to_string(),
                        size: 8_214,
                        mime_type: Some("application/xml".to_string()),
                        updated: ago(60 * 5),
                    }],
                ),
                (
                    maps,
                    vec![
                        ProfileFile {
                            id: 42,
                            name: "maps/osm.xml".to_string(),
                            size: 1_104,
                            mime_type: Some("application/xml".to_string()),
                            updated: ago(60 * 24 * 40),
                        },
                        ProfileFile {
                            id: 43,
                            name: "support/readme.txt".to_string(),
                            size: 302,
                            mime_type: Some("text/plain".to_string()),
                            updated: ago(60 * 24 * 40),
                        },
                    ],
                ),
            ],
            next_id: 300,
        }
    }

    fn take_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// Keeps the counts on the row honest after an edit, because they are what
    /// the list page shows and a stale one would be a demo that lies.
    fn recount(&mut self, id: ProfileId) {
        let prefs = self
            .prefs
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, entries)| entries.len())
            .unwrap_or_default();
        let files = self
            .files
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, entries)| entries.len())
            .unwrap_or_default();

        if let Some(profile) = self.profiles.iter_mut().find(|profile| profile.id == id) {
            profile.pref_count = prefs as u32;
            profile.file_count = files as u32;
            profile.updated = chrono::Utc::now();
        }
    }
}

pub fn profiles() -> Vec<Profile> {
    with(|state| state.profiles.clone())
}

pub fn profile(id: ProfileId) -> Result<Profile, ApiError> {
    with(|state| {
        state
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(missing)
    })
}

pub fn create_profile(request: &ProfileCreate) -> Result<Profile, ApiError> {
    with(|state| {
        if request.name.trim().is_empty() {
            return Err(ApiError::Server("A profile needs a name.".to_string()));
        }

        if state
            .profiles
            .iter()
            .any(|profile| profile.name.eq_ignore_ascii_case(request.name.trim()))
        {
            return Err(ApiError::Server(
                "There is already a profile with that name.".to_string(),
            ));
        }

        let id = ProfileId::new(state.take_id());
        let profile = Profile {
            id,
            name: request.name.trim().to_string(),
            description: request.description.clone(),
            active: request.active.unwrap_or(true),
            apply_on_enrollment: request.apply_on_enrollment,
            apply_on_connect: request.apply_on_connect,
            tool: request.tool.clone(),
            kind: request.kind.clone(),
            groups: request.groups.clone(),
            updated: chrono::Utc::now(),
            file_count: 0,
            pref_count: 0,
        };

        state.profiles.push(profile.clone());
        state.prefs.push((id, Vec::new()));
        state.files.push((id, Vec::new()));

        Ok(profile)
    })
}

pub fn patch_profile(id: ProfileId, change: &ProfileUpdate) -> Result<Profile, ApiError> {
    with(|state| {
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(missing)?;

        if let Some(description) = &change.description {
            profile.description = Some(description.clone());
        }
        if let Some(active) = change.active {
            profile.active = active;
        }
        if let Some(value) = change.apply_on_enrollment {
            profile.apply_on_enrollment = value;
        }
        if let Some(value) = change.apply_on_connect {
            profile.apply_on_connect = value;
        }
        if let Some(tool) = &change.tool {
            profile.tool = Some(tool.clone()).filter(|tool| !tool.trim().is_empty());
        }
        if let Some(kind) = &change.kind {
            profile.kind = Some(kind.clone());
        }
        if let Some(groups) = &change.groups {
            profile.groups = groups.clone();
        }

        profile.updated = chrono::Utc::now();

        Ok(profile.clone())
    })
}

pub fn delete_profile(id: ProfileId) -> Result<(), ApiError> {
    with(|state| {
        let before = state.profiles.len();
        state.profiles.retain(|profile| profile.id != id);
        state.prefs.retain(|(held, _)| *held != id);
        state.files.retain(|(held, _)| *held != id);

        match state.profiles.len() < before {
            true => Ok(()),
            false => Err(missing()),
        }
    })
}

pub fn profile_prefs(id: ProfileId) -> Result<Vec<PrefEntry>, ApiError> {
    with(|state| {
        state
            .prefs
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, entries)| entries.clone())
            .ok_or_else(missing)
    })
}

pub fn set_profile_prefs(id: ProfileId, entries: &[PrefEntry]) -> Result<Vec<PrefEntry>, ApiError> {
    for entry in entries {
        if entry.key.trim().is_empty() {
            return Err(ApiError::Server("A preference needs a key.".to_string()));
        }

        if !entry.class.accepts(&entry.value) {
            return Err(ApiError::Server(format!(
                "'{}' is not a {} value.",
                entry.value,
                entry.class.as_str(),
            )));
        }
    }

    with(|state| {
        let held = state
            .prefs
            .iter_mut()
            .find(|(held, _)| *held == id)
            .ok_or_else(missing)?;

        held.1 = entries.to_vec();
        state.recount(id);

        Ok(entries.to_vec())
    })
}

pub fn profile_files(id: ProfileId) -> Result<Vec<ProfileFile>, ApiError> {
    with(|state| {
        state
            .files
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, entries)| entries.clone())
            .ok_or_else(missing)
    })
}

pub fn add_profile_file(id: ProfileId, name: String, size: u64) -> Result<ProfileFile, ApiError> {
    with(|state| {
        let file_id = state.take_id();
        let held = state
            .files
            .iter_mut()
            .find(|(held, _)| *held == id)
            .ok_or_else(missing)?;

        let file = ProfileFile {
            id: file_id,
            name,
            size,
            mime_type: None,
            updated: chrono::Utc::now(),
        };

        held.1.retain(|existing| existing.name != file.name);
        held.1.push(file.clone());
        state.recount(id);

        Ok(file)
    })
}

pub fn delete_profile_file(id: ProfileId, file: i64) -> Result<(), ApiError> {
    with(|state| {
        let held = state
            .files
            .iter_mut()
            .find(|(held, _)| *held == id)
            .ok_or_else(missing)?;

        let before = held.1.len();
        held.1.retain(|existing| existing.id != file);
        let removed = held.1.len() < before;
        state.recount(id);

        match removed {
            true => Ok(()),
            false => Err(ApiError::Server(
                "There is no such file on that profile.".to_string(),
            )),
        }
    })
}

/// The zip a device would receive.
///
/// Demo mode has no package builder behind it — assembling a real Mission
/// Package is the server's job and duplicating it here would be a second
/// implementation to keep honest — so this is a valid but empty archive. The
/// page shows its name and size, which is what the button is for.
pub fn profile_preview(id: ProfileId) -> Result<Download, ApiError> {
    let profile = profile(id)?;

    if profile.pref_count == 0 && profile.file_count == 0 {
        return Err(ApiError::Server(
            "That profile has no preferences and no files, so a device would receive nothing."
                .to_string(),
        ));
    }

    Ok(Download {
        filename: "profile.zip".to_string(),
        mime: "application/zip".to_string(),
        bytes: empty_zip(),
    })
}

/// The catalogue the preference editor autocompletes from.
///
/// A handful of the server's twenty, chosen so the editor's behaviour — the
/// class arriving with the key, and ATAK's own default shown beside it — can
/// be seen without a server.
pub fn pref_catalog() -> Vec<PrefCatalogEntry> {
    let entry =
        |key: &str, class: PrefClass, description: &str, default: Option<&str>| PrefCatalogEntry {
            key: key.to_string(),
            class,
            description: description.to_string(),
            default: default.map(str::to_string),
        };

    vec![
        entry(
            "deviceProfileEnableOnConnect",
            PrefClass::String,
            "Fetch connection and tool profiles on every stream connect. Off in ATAK by \
             default, so nothing else on this list is delivered on connect until it is on.",
            Some("false"),
        ),
        entry(
            "displayServerConnectionWidget",
            PrefClass::String,
            "Show the server connection indicator on the map.",
            Some("false"),
        ),
        entry(
            "prefs_enable_channels",
            PrefClass::String,
            "Show the Channels selector, which is how somebody chooses which channels they \
             transmit on.",
            Some("false"),
        ),
        entry(
            "locationCallsign",
            PrefClass::String,
            "The callsign other people see on their maps.",
            None,
        ),
        entry(
            "locationTeam",
            PrefClass::String,
            "The team colour, as a word: Cyan, Green, Blue, and so on.",
            Some("Cyan"),
        ),
        entry(
            "atakRoleType",
            PrefClass::String,
            "The role shown beside the callsign: Team Member, Team Lead, HQ, and so on.",
            Some("Team Member"),
        ),
        entry(
            "coord_display_pref",
            PrefClass::String,
            "Which coordinate format the map shows: MGRS, DD, DM, DMS or UTM.",
            Some("MGRS"),
        ),
        entry(
            "alt_display_pref",
            PrefClass::String,
            "Whether altitude is shown above mean sea level (MSL) or the ellipsoid (HAE).",
            Some("MSL"),
        ),
        entry(
            "locationReportingStrategy",
            PrefClass::String,
            "Whether position reports are sent on a timer or when the device moves.",
            Some("Dynamic"),
        ),
        entry(
            "dynamicReportingRateMinReliable",
            PrefClass::Integer,
            "The shortest gap, in seconds, between two position reports.",
            Some("20"),
        ),
    ]
}
