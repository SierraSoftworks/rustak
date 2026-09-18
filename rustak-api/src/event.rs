//! The server-event feed: what `GET /api/v1/events` publishes, and what a
//! sidecar reads back off it.
//!
//! A plugin that wants to react to what happens on the server — a device coming
//! on the stream, a mission changing, a package arriving — would otherwise have
//! to poll for it. The feed is the alternative: one long-lived Server-Sent
//! Events response carrying [`ServerEvent`]s as they happen, resumable from the
//! last one a client saw.
//!
//! # The shape on the wire
//!
//! Each event is one SSE frame whose `event:` field is the event's
//! [`name`](ServerEvent::name), whose `id:` field is its
//! [`id`](ServerEvent::id), and whose `data:` field is this type as JSON:
//!
//! ```text
//! id: 41
//! event: client.connected
//! data: {"id":41,"at":"2026-09-18T12:00:00.250Z","type":"client.connected","username":"ada","uid":"ANDROID-1","callsign":"ADA"}
//! ```
//!
//! The name appears twice on purpose. A browser's `EventSource` dispatches on
//! the `event:` field and never sees the body's `type`; a Rust client parses the
//! body and would otherwise have to carry the frame's field alongside it.
//!
//! # Nothing here is a notification
//!
//! The feed says that something changed, not what it now is. A consumer that
//! needs the new state reads it back through the API that owns it, which is
//! what keeps this crate from having to mirror every other module's model — and
//! what keeps an event small enough that a slow consumer falls behind by
//! kilobytes rather than megabytes.
//!
//! Nothing secret is carried: no tokens, no certificate material, no file
//! contents, and no peer addresses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::ServiceName;
use crate::service::ServiceState;

/// `skip_serializing_if` for a flag that is usually false.
fn is_false(value: &bool) -> bool {
    !*value
}

/// One thing that happened, as the feed reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEvent {
    /// Monotonic within one server process, and what `Last-Event-ID` resumes
    /// from. It restarts at 1 when the server does, which a consumer notices as
    /// an id lower than the one it last saw.
    pub id: u64,

    /// When it happened.
    pub at: DateTime<Utc>,

    /// What happened.
    #[serde(flatten)]
    pub payload: ServerEventPayload,
}

impl ServerEvent {
    /// The event's name, which is also the SSE `event:` field.
    pub fn name(&self) -> &'static str {
        self.payload.name()
    }
}

/// What happened, and what it happened to.
///
/// `#[non_exhaustive]`, because the set of things worth announcing grows: match
/// it with a `_` arm and a new variant is an additive change rather than a
/// broken build. A client that cannot parse a variant it has never heard of
/// skips the frame rather than ending the stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ServerEventPayload {
    /// A client authenticated and joined the CoT stream.
    #[serde(rename = "client.connected")]
    ClientConnected(ClientEvent),

    /// A client's CoT stream connection ended.
    #[serde(rename = "client.disconnected")]
    ClientDisconnected(ClientEvent),

    /// A mission was created, changed, deleted or shared.
    #[serde(rename = "mission.changed")]
    MissionChanged(MissionEvent),

    /// An account's channel membership or selection changed.
    #[serde(rename = "channel.changed")]
    ChannelChanged(ChannelEvent),

    /// A file or mission package arrived in enterprise sync.
    #[serde(rename = "package.uploaded")]
    PackageUploaded(PackageEvent),

    /// A registered service reported its health, or stopped reporting it.
    #[serde(rename = "service.status")]
    ServiceStatus(ServiceEvent),
}

impl ServerEventPayload {
    /// The name this payload is published under.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ClientConnected(_) => "client.connected",
            Self::ClientDisconnected(_) => "client.disconnected",
            Self::MissionChanged(_) => "mission.changed",
            Self::ChannelChanged(_) => "channel.changed",
            Self::PackageUploaded(_) => "package.uploaded",
            Self::ServiceStatus(_) => "service.status",
        }
    }
}

/// A client arriving on, or leaving, the CoT stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEvent {
    /// The account it authenticated as.
    pub username: String,

    /// The uid it calls itself, once it has said. Absent for a connection that
    /// ended before it sent anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,

    /// Its callsign, once it has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
}

/// Something that happened to a mission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionEvent {
    /// The mission's name, which is how the Marti API addresses it.
    pub name: String,

    /// Its guid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,

    /// The `t-x-m-*` type of the notice this came from, which says what kind of
    /// change it was.
    pub change: String,

    /// The uid of whoever caused it, when they named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_uid: Option<String>,
}

/// An account's channels changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelEvent {
    /// Whose channels changed.
    pub username: String,
}

/// A file arrived in enterprise sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageEvent {
    /// The resource uid, which is how it is fetched back.
    pub uid: String,

    /// What it is called.
    pub name: String,

    /// Its content hash, which is the other way to fetch it.
    pub hash: String,

    /// How many bytes arrived.
    pub size: i64,

    /// Whether it is a mission package rather than a loose file.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mission_package: bool,

    /// Who uploaded it, when the upload was authenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter: Option<String>,
}

/// A service said how it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEvent {
    /// Which service.
    pub name: ServiceName,

    /// What it said.
    pub state: ServiceState,

    /// Why, in its own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(payload: ServerEventPayload) -> ServerEvent {
        ServerEvent {
            id: 41,
            at: "2026-09-18T12:00:00.250Z".parse().unwrap(),
            payload,
        }
    }

    #[test]
    fn an_event_carries_its_name_in_the_body_as_well_as_the_frame() {
        // The `event:` SSE field is what a browser dispatches on and the body's
        // `type` is what a Rust client matches on; both have to say the same
        // thing or the two halves of the feed disagree.
        let connected = event(ServerEventPayload::ClientConnected(ClientEvent {
            username: "ada".into(),
            uid: Some("ANDROID-1".into()),
            callsign: Some("ADA".into()),
        }));

        let json = serde_json::to_value(&connected).unwrap();

        assert_eq!(connected.name(), "client.connected");
        assert_eq!(json["type"], "client.connected");
        assert_eq!(json["id"], 41);
        assert_eq!(json["username"], "ada");
        assert_eq!(
            serde_json::from_value::<ServerEvent>(json).unwrap(),
            connected
        );
    }

    #[test]
    fn every_payload_round_trips_under_its_published_name() {
        let payloads = [
            (
                "client.disconnected",
                ServerEventPayload::ClientDisconnected(ClientEvent {
                    username: "ada".into(),
                    ..ClientEvent::default()
                }),
            ),
            (
                "mission.changed",
                ServerEventPayload::MissionChanged(MissionEvent {
                    name: "OPERATION-X".into(),
                    guid: Some("2c1e…".into()),
                    change: "t-x-m-c".into(),
                    author_uid: Some("ANDROID-1".into()),
                }),
            ),
            (
                "channel.changed",
                ServerEventPayload::ChannelChanged(ChannelEvent {
                    username: "ada".into(),
                }),
            ),
            (
                "package.uploaded",
                ServerEventPayload::PackageUploaded(PackageEvent {
                    uid: "res-1".into(),
                    name: "map.zip".into(),
                    hash: "abc".into(),
                    size: 12,
                    mission_package: true,
                    submitter: Some("ada".into()),
                }),
            ),
            (
                "service.status",
                ServerEventPayload::ServiceStatus(ServiceEvent {
                    name: ServiceName::parse("weather").unwrap(),
                    state: ServiceState::Degraded,
                    message: Some("Upstream is slow.".into()),
                }),
            ),
        ];

        for (name, payload) in payloads {
            let original = event(payload);
            let json = serde_json::to_string(&original).unwrap();

            assert_eq!(original.name(), name, "{json}");
            assert!(json.contains(&format!(r#""type":"{name}""#)), "{json}");
            assert_eq!(
                serde_json::from_str::<ServerEvent>(&json).unwrap(),
                original
            );
        }
    }

    #[test]
    fn an_event_a_client_has_never_heard_of_is_a_parse_failure_rather_than_a_panic() {
        // What forward compatibility costs: a consumer skips the frame and
        // keeps the connection, which is what `control::events` does.
        let unknown = r#"{"id":1,"at":"2026-09-18T12:00:00.250Z","type":"weather.rained"}"#;

        assert!(serde_json::from_str::<ServerEvent>(unknown).is_err());
    }

    #[test]
    fn nothing_optional_is_emitted_when_there_is_nothing_to_say() {
        // A feed frame is written once per consumer per event, so the quiet
        // case is the one that has to stay small.
        let quiet = event(ServerEventPayload::ClientDisconnected(ClientEvent {
            username: "ada".into(),
            ..ClientEvent::default()
        }));

        assert_eq!(
            serde_json::to_string(&quiet).unwrap(),
            r#"{"id":41,"at":"2026-09-18T12:00:00.250Z","type":"client.disconnected","username":"ada"}"#
        );
    }
}
