//! What is connected to the CoT stream right now, and what used to be.
//!
//! A [`ConnectedClient`] is a *connection*, not a device row: it exists for as
//! long as a socket does, its callsign and team come out of the situational
//! awareness the client sends rather than out of the database, and it
//! disappears the moment the connection closes. That is why
//! [`ClientHistoryEntry`] is a separate type — the question "who is here?" and
//! the question "who has been here?" have different answers, different sources
//! and different freshness.
//!
//! # Channels are listed in both directions
//!
//! A connection's `OUT` channels decide what reaches it and its `IN` channels
//! decide where what it sends goes. An operator looking at a client that cannot
//! see anybody needs to know which of the two is empty, so both are rendered
//! rather than the single list the TAK wire format carries.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::GroupName;

/// One live connection to the CoT stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedClient {
    /// The uid the client calls itself, from the first message it sent that
    /// carried a contact endpoint.
    pub client_uid: String,

    /// The name shown on other people's maps.
    pub callsign: String,

    /// The account its certificate authenticated as.
    pub username: String,

    /// Its team colour, from `<__group name>`; `unknown` until it says.
    pub team: String,

    /// Its role, from `<__group role>`; `unknown` until it says.
    pub role: String,

    /// `platform:version`, from `<takv>`; `unknown` until it says.
    pub takv: String,

    /// How it is connected: `tls` for a mutually authenticated stream.
    pub protocol: String,

    /// Where it connected from.
    pub ip: IpAddr,

    /// The port it connected from, which is what tells two connections from
    /// one host apart.
    pub port: u16,

    pub connected_at: DateTime<Utc>,

    /// When it was last heard from, which is what a stalled client looks like.
    pub last_event_at: DateTime<Utc>,

    /// Whether it has asked to be invisible to implicit broadcast.
    pub incognito: bool,

    /// The channels it may publish into.
    #[serde(default)]
    pub in_groups: Vec<GroupName>,

    /// The channels it receives from.
    #[serde(default)]
    pub out_groups: Vec<GroupName>,
}

/// Turning one client's visibility on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncognitoRequest {
    /// `true` hides the client from implicit broadcast; `false` reveals it.
    pub on: bool,
}

/// What the stream listener itself is doing.
///
/// `GET /clients` answers `[]` both for "the listener is running and nobody is
/// connected" and for "this installation has no listener", and those are
/// different things for an operator to be told: the first is quiet, the second
/// is a configuration problem. This is the field that tells them apart, so no
/// page has to render the ambiguous sentence.
///
/// Administrative, like the rest of `/clients` — how many devices are on an
/// installation is not something an unauthenticated caller learns, which is
/// also why it is not on the public health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStatus {
    /// Whether the configuration switches the listener on at all.
    pub enabled: bool,

    /// Whether it has bound and published its registry. `false` with `enabled`
    /// true means it was asked for and did not come up.
    pub bound: bool,

    /// How many connections are open right now.
    pub connections: u32,
}

impl StreamStatus {
    /// What an installation with `[stream.tls] enabled = false` answers.
    pub fn off() -> Self {
        Self {
            enabled: false,
            bound: false,
            connections: 0,
        }
    }

    /// Whether an empty client list means "quiet" rather than "switched off".
    pub fn is_listening(&self) -> bool {
        self.enabled && self.bound
    }
}

/// One client this server has seen, whether or not it is connected now.
///
/// The enrolled device is the source, so a client that has never connected
/// since enrolling still appears — with `connected` false and whatever the
/// last stored situational-awareness message said about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHistoryEntry {
    pub client_uid: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    pub username: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// `platform version`, when the client told us either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takv: Option<String>,

    pub first_seen_at: DateTime<Utc>,

    pub last_seen_at: DateTime<Utc>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ip: Option<IpAddr>,

    /// Whether a connection under this uid is open right now.
    pub connected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str) -> GroupName {
        GroupName::parse(name).unwrap()
    }

    fn connected() -> ConnectedClient {
        ConnectedClient {
            client_uid: "ANDROID-1".to_string(),
            callsign: "ALPHA".to_string(),
            username: "grace".to_string(),
            team: "Cyan".to_string(),
            role: "Team Member".to_string(),
            takv: "ATAK-CIV:5.6.0".to_string(),
            protocol: "tls".to_string(),
            ip: "198.51.100.7".parse().unwrap(),
            port: 41234,
            connected_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            last_event_at: "2026-09-18T12:04:00.000Z".parse().unwrap(),
            incognito: false,
            in_groups: vec![group("Blue")],
            out_groups: vec![group("Blue"), group("__ANON__")],
        }
    }

    #[test]
    fn a_connected_client_round_trips_through_serde() {
        let json = serde_json::to_string(&connected()).unwrap();

        assert_eq!(
            serde_json::from_str::<ConnectedClient>(&json).unwrap(),
            connected()
        );
    }

    #[test]
    fn both_channel_directions_are_present_even_when_empty() {
        // A client that cannot see anybody has an empty `out_groups`; an absent
        // key would read as "we do not know", which is a different answer.
        let json = serde_json::to_value(ConnectedClient {
            in_groups: Vec::new(),
            out_groups: Vec::new(),
            ..connected()
        })
        .unwrap();

        assert_eq!(json["in_groups"], serde_json::json!([]));
        assert_eq!(json["out_groups"], serde_json::json!([]));
        assert_eq!(json["ip"], "198.51.100.7");
        assert_eq!(json["port"], 41234);
    }

    #[test]
    fn an_incognito_request_is_a_value_rather_than_a_toggle() {
        // The Marti endpoint toggles, because the client asking is the one that
        // knows what it is now. An operator's page does not, so this one says
        // which state it wants.
        let parsed: IncognitoRequest = serde_json::from_str(r#"{"on":true}"#).unwrap();

        assert!(parsed.on);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), r#"{"on":true}"#);
    }

    #[test]
    fn an_empty_client_list_is_only_ambiguous_without_this() {
        // "Nobody is connected" and "there is no listener" are different things
        // for an operator to be told, and `GET /clients` answers `[]` for both.
        let quiet = StreamStatus {
            enabled: true,
            bound: true,
            connections: 0,
        };
        assert!(quiet.is_listening());

        let off = StreamStatus::off();
        assert!(!off.is_listening());
        assert_eq!(off.connections, 0);

        let asked_for_and_absent = StreamStatus {
            enabled: true,
            bound: false,
            connections: 0,
        };
        assert!(
            !asked_for_and_absent.is_listening(),
            "configured but not bound is not listening either",
        );

        let json = serde_json::to_string(&quiet).unwrap();
        assert_eq!(json, r#"{"enabled":true,"bound":true,"connections":0}"#);
        assert_eq!(serde_json::from_str::<StreamStatus>(&json).unwrap(), quiet);
    }

    #[test]
    fn a_history_entry_round_trips_and_omits_what_was_never_said() {
        let entry = ClientHistoryEntry {
            client_uid: "ANDROID-2".to_string(),
            callsign: None,
            username: "ada".to_string(),
            team: None,
            role: None,
            takv: None,
            first_seen_at: "2026-09-17T09:00:00.000Z".parse().unwrap(),
            last_seen_at: "2026-09-18T09:00:00.000Z".parse().unwrap(),
            last_ip: None,
            connected: false,
        };

        let json = serde_json::to_value(&entry).unwrap();

        assert!(json.get("callsign").is_none());
        assert!(json.get("last_ip").is_none());
        assert_eq!(json["connected"], false);
        assert_eq!(
            serde_json::from_value::<ClientHistoryEntry>(json).unwrap(),
            entry
        );
    }
}
