//! Demo data and behaviour for data packages, connected clients and the CoT
//! browser.
//!
//! Three subjects in one store because they are one subject in practice: the
//! fixture clients are the fixture devices, the CoT the browser lists is what
//! those clients sent, and a package's channels are the same four channels
//! everything else here uses. Splitting them would mean three stores whose
//! consistency nobody enforced.

use std::cell::RefCell;

use rustak_api::{
    ClientHistoryEntry, ConnectedClient, CotDetail, CotSummary, GroupName, PackageSummary,
    PackageUpdate,
};

use super::data::ago;
use super::empty_zip;
use crate::api::ApiError;
use crate::api::cot::CotFilter;
use crate::api::download::Download;
use crate::api::packages::PackageFilter;

struct State {
    packages: Vec<PackageSummary>,
    clients: Vec<ConnectedClient>,
    cot: Vec<(CotDetail, Vec<CotSummary>)>,
    next: u32,
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

/// A hash that looks like one without being one: every fixture secret in this
/// crate names itself as fake.
fn hash(seed: &str) -> String {
    let mut value = format!("{seed}{}", "0".repeat(64));
    value.truncate(64);
    value
}

fn event(uid: &str, kind: &str, callsign: &str, lat: f64, lon: f64) -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<event version=\"2.0\" uid=\"{uid}\" type=\"{kind}\"\n",
            "       time=\"2026-09-18T12:00:00.000Z\" start=\"2026-09-18T12:00:00.000Z\"\n",
            "       stale=\"2026-09-18T12:02:00.000Z\" how=\"m-g\">\n",
            "  <point lat=\"{lat}\" lon=\"{lon}\" hae=\"12.4\" ce=\"9.5\" le=\"9999999.0\"/>\n",
            "  <detail>\n",
            "    <contact callsign=\"{callsign}\" endpoint=\"*:-1:stcp\"/>\n",
            "    <__group name=\"Cyan\" role=\"Team Member\"/>\n",
            "    <takv device=\"Pixel 8\" platform=\"ATAK-CIV\" os=\"34\" version=\"5.2.0\"/>\n",
            "    <status battery=\"74\"/>\n",
            "  </detail>\n",
            "</event>",
        ),
        uid = uid,
        kind = kind,
        callsign = callsign,
        lat = lat,
        lon = lon,
    )
}

fn summary(
    uid: &str,
    kind: &str,
    callsign: &str,
    minutes: i64,
    lat: f64,
    lon: f64,
    groups: &[&str],
) -> CotSummary {
    CotSummary {
        uid: uid.to_string(),
        kind: kind.to_string(),
        callsign: Some(callsign.to_string()),
        team: Some("Cyan".to_string()),
        role: Some("Team Member".to_string()),
        time: ago(minutes),
        // Ten minutes after it was sent, so a recent report is current and an
        // older one has gone stale — the distinction the page exists to draw.
        stale: ago(minutes - 10),
        received_at: ago(minutes),
        lat,
        lon,
        groups: groups.iter().map(|name| (*name).to_string()).collect(),
    }
}

impl State {
    fn new() -> Self {
        let kettle = summary(
            "ANDROID-2f1c9a7b4e0d",
            "a-f-G-U-C",
            "QUINN",
            2,
            51.50735,
            -0.12776,
            &["Command", "Blue Team"],
        );
        let rao = summary(
            "IOS-91ac4d55f207",
            "a-f-G-U-C",
            "RAO",
            9,
            51.5155,
            -0.0922,
            &["Blue Team"],
        );
        let marker = summary(
            "MARKER-7c19d0",
            "a-h-G",
            "CONTACT 1",
            41,
            51.4975,
            -0.1357,
            &["Command"],
        );

        Self {
            packages: vec![
                PackageSummary {
                    hash: hash("demofa11"),
                    uid: "3f6c1c0e-0f0a-4a1a-9f3b-000000000001".to_string(),
                    name: "Patrol brief.zip".to_string(),
                    filename: Some("patrol-brief.zip".to_string()),
                    mime_type: "application/x-zip-compressed".to_string(),
                    size: 4_194_304,
                    submitter: Some("avery".to_string()),
                    creator_uid: Some("ANDROID-2f1c9a7b4e0d".to_string()),
                    submission_time: ago(60 * 26),
                    keywords: vec!["missionpackage".to_string(), "brief".to_string()],
                    groups: vec!["Command".to_string()],
                    tool: "public".to_string(),
                    expiration: None,
                    install_on_enrollment: false,
                    mission_package: true,
                    mission_name: Some("Operation Kettle".to_string()),
                },
                PackageSummary {
                    hash: hash("demofa22"),
                    uid: "3f6c1c0e-0f0a-4a1a-9f3b-000000000002".to_string(),
                    name: "Radio plan.pdf".to_string(),
                    filename: Some("radio-plan.pdf".to_string()),
                    mime_type: "application/pdf".to_string(),
                    size: 812_004,
                    submitter: Some("bhavna".to_string()),
                    creator_uid: None,
                    submission_time: ago(60 * 5),
                    keywords: Vec::new(),
                    groups: vec!["Blue Team".to_string(), "Command".to_string()],
                    tool: "public".to_string(),
                    expiration: None,
                    install_on_enrollment: false,
                    mission_package: false,
                    mission_name: None,
                },
                PackageSummary {
                    hash: hash("demofa33"),
                    uid: "3f6c1c0e-0f0a-4a1a-9f3b-000000000003".to_string(),
                    name: "Standard iconset.zip".to_string(),
                    filename: Some("iconset.zip".to_string()),
                    mime_type: "application/x-zip-compressed".to_string(),
                    size: 2_097_152,
                    submitter: Some("avery".to_string()),
                    creator_uid: None,
                    submission_time: ago(60 * 24 * 30),
                    keywords: vec!["missionpackage".to_string(), "iconset".to_string()],
                    groups: Vec::new(),
                    tool: "public".to_string(),
                    expiration: Some(ago(-60 * 24 * 90)),
                    // The one every device is handed on enrolment, so the flag
                    // has somewhere to be true.
                    install_on_enrollment: true,
                    mission_package: true,
                    mission_name: None,
                },
            ],
            clients: vec![
                ConnectedClient {
                    client_uid: "ANDROID-2f1c9a7b4e0d".to_string(),
                    callsign: "QUINN".to_string(),
                    username: "avery".to_string(),
                    team: "Cyan".to_string(),
                    role: "Team Member".to_string(),
                    takv: "ATAK-CIV:5.2.0".to_string(),
                    protocol: "tls".to_string(),
                    ip: "203.0.113.24".parse().expect("a fixture address"),
                    port: 41234,
                    connected_at: ago(190),
                    last_event_at: ago(1),
                    incognito: false,
                    in_groups: vec![group("Command"), group("Blue Team")],
                    out_groups: vec![group("Command")],
                },
                ConnectedClient {
                    client_uid: "IOS-91ac4d55f207".to_string(),
                    callsign: "RAO".to_string(),
                    username: "bhavna".to_string(),
                    team: "Green".to_string(),
                    role: "Team Lead".to_string(),
                    takv: "iTAK:2.9.4".to_string(),
                    protocol: "tls".to_string(),
                    ip: "198.51.100.19".parse().expect("a fixture address"),
                    port: 52118,
                    connected_at: ago(48),
                    last_event_at: ago(9),
                    incognito: true,
                    in_groups: vec![group("Blue Team")],
                    out_groups: vec![group("Blue Team")],
                },
            ],
            cot: vec![
                (
                    CotDetail {
                        xml: event(
                            "ANDROID-2f1c9a7b4e0d",
                            "a-f-G-U-C",
                            "QUINN",
                            51.50735,
                            -0.12776,
                        ),
                        summary: kettle.clone(),
                    },
                    vec![
                        kettle.clone(),
                        summary(
                            "ANDROID-2f1c9a7b4e0d",
                            "a-f-G-U-C",
                            "QUINN",
                            14,
                            51.5062,
                            -0.1281,
                            &["Command", "Blue Team"],
                        ),
                        summary(
                            "ANDROID-2f1c9a7b4e0d",
                            "a-f-G-U-C",
                            "QUINN",
                            31,
                            51.5041,
                            -0.1295,
                            &["Command", "Blue Team"],
                        ),
                    ],
                ),
                (
                    CotDetail {
                        xml: event("IOS-91ac4d55f207", "a-f-G-U-C", "RAO", 51.5155, -0.0922),
                        summary: rao.clone(),
                    },
                    vec![rao.clone()],
                ),
                (
                    CotDetail {
                        xml: event("MARKER-7c19d0", "a-h-G", "CONTACT 1", 51.4975, -0.1357),
                        summary: marker.clone(),
                    },
                    vec![marker.clone()],
                ),
            ],
            next: 0,
        }
    }

    fn take(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
}

fn matches_package(package: &PackageSummary, filter: &PackageFilter) -> bool {
    if filter.mission_package && !package.mission_package {
        return false;
    }

    let needle = filter.query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }

    package.name.to_lowercase().contains(&needle)
        || package
            .keywords
            .iter()
            .any(|keyword| keyword.to_lowercase().contains(&needle))
}

pub fn packages(filter: &PackageFilter) -> Vec<PackageSummary> {
    with(|state| {
        state
            .packages
            .iter()
            .filter(|package| matches_package(package, filter))
            .cloned()
            .collect()
    })
}

pub fn patch_package(hash: &str, change: &PackageUpdate) -> Result<PackageSummary, ApiError> {
    with(|state| {
        let package = state
            .packages
            .iter_mut()
            .find(|package| package.hash == hash)
            .ok_or_else(|| ApiError::Server("There is no such package.".to_string()))?;

        if let Some(name) = &change.name {
            if name.trim().is_empty() {
                return Err(ApiError::Server("A package needs a name.".to_string()));
            }
            package.name = name.trim().to_string();
        }
        if let Some(tool) = &change.tool {
            package.tool = tool.clone();
        }
        if let Some(groups) = &change.groups {
            package.groups = groups.clone();
        }
        if let Some(keywords) = &change.keywords {
            package.keywords = keywords.clone();
            // The indexed copy has to move with the keyword, or a client's
            // data-package browser keeps listing a file that no longer says it
            // belongs there.
            package.mission_package = keywords.iter().any(|word| word == "missionpackage");
        }
        if let Some(install) = change.install_on_enrollment {
            package.install_on_enrollment = install;
        }
        if let Some(expiration) = change.expiration {
            // `Some(None)` is an explicit `null`, which clears it.
            package.expiration = expiration;
        }

        Ok(package.clone())
    })
}

pub fn delete_package(hash: &str) -> Result<(), ApiError> {
    with(|state| {
        let before = state.packages.len();
        state.packages.retain(|package| package.hash != hash);

        match state.packages.len() < before {
            true => Ok(()),
            false => Err(ApiError::Server("There is no such package.".to_string())),
        }
    })
}

/// Demo mode has no content store behind it, so this is a valid but empty
/// archive named the way the real one would be.
pub fn package_content(package: &PackageSummary) -> Result<Download, ApiError> {
    Ok(Download {
        filename: package
            .filename
            .clone()
            .unwrap_or_else(|| package.name.clone()),
        mime: package.mime_type.clone(),
        bytes: empty_zip(),
    })
}

pub fn create_package(
    name: String,
    size: i64,
    tool: Option<&str>,
    keywords: &[String],
    groups: &[String],
) -> Result<PackageSummary, ApiError> {
    with(|state| {
        let serial = state.take();
        let package = PackageSummary {
            hash: hash(&format!("demoadd{serial}")),
            uid: format!("3f6c1c0e-0f0a-4a1a-9f3b-0000000001{serial:02}"),
            name,
            filename: None,
            mime_type: "application/octet-stream".to_string(),
            size,
            submitter: Some("avery".to_string()),
            creator_uid: None,
            submission_time: chrono::Utc::now(),
            mission_package: keywords.iter().any(|word| word == "missionpackage"),
            keywords: keywords.to_vec(),
            groups: groups.to_vec(),
            tool: tool.unwrap_or("public").to_string(),
            expiration: None,
            install_on_enrollment: false,
            mission_name: None,
        };

        state.packages.insert(0, package.clone());

        Ok(package)
    })
}

pub fn clients() -> Vec<ConnectedClient> {
    with(|state| state.clients.clone())
}

/// Every device seen recently, connected or not.
///
/// The fixture devices that are not in [`clients`] appear here as disconnected,
/// which is what makes the two lists worth showing side by side.
pub fn client_history(_secago: i64) -> Vec<ClientHistoryEntry> {
    let live = clients();
    let entry = |device: &rustak_api::Device| ClientHistoryEntry {
        client_uid: device.uid.to_string(),
        callsign: device.callsign.clone(),
        username: device.username.to_string(),
        team: Some("Cyan".to_string()),
        role: Some("Team Member".to_string()),
        takv: device.platform_version(),
        first_seen_at: device.first_seen_at,
        last_seen_at: device.last_seen_at,
        last_ip: device.last_ip,
        connected: live
            .iter()
            .any(|client| client.client_uid == device.uid.to_string()),
    };

    super::devices(None).iter().map(entry).collect()
}

pub fn disconnect_client(client_uid: &str) -> Result<(), ApiError> {
    with(|state| {
        let before = state.clients.len();
        state
            .clients
            .retain(|client| client.client_uid != client_uid);

        match state.clients.len() < before {
            true => Ok(()),
            false => Err(ApiError::Server(
                "Nothing is connected under that identifier.".to_string(),
            )),
        }
    })
}

pub fn set_incognito(client_uid: &str, on: bool) -> Result<ConnectedClient, ApiError> {
    with(|state| {
        let client = state
            .clients
            .iter_mut()
            .find(|client| client.client_uid == client_uid)
            .ok_or_else(|| {
                ApiError::Server("Nothing is connected under that identifier.".to_string())
            })?;

        client.incognito = on;

        Ok(client.clone())
    })
}

/// The demo installation has a listener, with the fixture clients on it.
pub fn stream_status() -> rustak_api::StreamStatus {
    let connections = clients().len();

    rustak_api::StreamStatus {
        enabled: true,
        bound: true,
        connections: u32::try_from(connections).unwrap_or(u32::MAX),
    }
}

fn matches_cot(summary: &CotSummary, filter: &CotFilter) -> bool {
    if !filter.kind.trim().is_empty() && !summary.kind.starts_with(filter.kind.trim()) {
        return false;
    }

    if !filter.callsign.trim().is_empty() {
        let needle = filter.callsign.trim().to_lowercase();
        if !summary
            .callsign
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains(&needle)
        {
            return false;
        }
    }

    match &filter.group {
        Some(group) => summary.groups.iter().any(|held| held == group.as_str()),
        None => true,
    }
}

pub fn cot(filter: &CotFilter) -> Vec<CotSummary> {
    with(|state| {
        state
            .cot
            .iter()
            .map(|(detail, _)| detail.summary.clone())
            .filter(|summary| matches_cot(summary, filter))
            .collect()
    })
}

pub fn cot_detail(uid: &str) -> Result<CotDetail, ApiError> {
    with(|state| {
        state
            .cot
            .iter()
            .find(|(detail, _)| detail.summary.uid == uid)
            .map(|(detail, _)| detail.clone())
            .ok_or_else(|| ApiError::Server("Nothing is stored for that identifier.".to_string()))
    })
}

pub fn cot_history(uid: &str, _secago: i64) -> Vec<CotSummary> {
    with(|state| {
        state
            .cot
            .iter()
            .find(|(detail, _)| detail.summary.uid == uid)
            .map(|(_, history)| history.clone())
            .unwrap_or_default()
    })
}

pub fn forget_cot(uid: &str) -> Result<(), ApiError> {
    with(|state| {
        let before = state.cot.len();
        state.cot.retain(|(detail, _)| detail.summary.uid != uid);

        match state.cot.len() < before {
            true => Ok(()),
            false => Err(ApiError::Server(
                "Nothing is stored for that identifier.".to_string(),
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_summary() -> CotSummary {
        summary(
            "ANDROID-1",
            "a-f-G-U-C",
            "ALPHA",
            1,
            51.5,
            -0.1,
            &["Command"],
        )
    }

    #[test]
    fn a_cot_type_filter_matches_a_prefix_rather_than_the_whole_type() {
        let filter = |kind: &str| CotFilter {
            kind: kind.to_string(),
            ..CotFilter::default()
        };

        assert!(matches_cot(&a_summary(), &filter("a-")));
        assert!(matches_cot(&a_summary(), &filter("a-f-G")));
        assert!(!matches_cot(&a_summary(), &filter("b-t-f")));
    }

    #[test]
    fn a_package_filter_reaches_the_keywords_as_well_as_the_name() {
        let package = with(|state| state.packages[0].clone());
        let query = |value: &str| PackageFilter {
            query: value.to_string(),
            ..PackageFilter::default()
        };

        assert!(matches_package(&package, &PackageFilter::default()));
        assert!(matches_package(&package, &query("PATROL")));
        assert!(matches_package(&package, &query("brief")));
        assert!(!matches_package(&package, &query("iconset")));
    }
}
