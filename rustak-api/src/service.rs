//! Sidecar services: what they tell us about themselves, and what we say about
//! them afterwards.
//!
//! A sidecar is a separate process that connects to this server the way any
//! other client does — it has an identity of kind
//! [`crate::user::UserKind::Service`], a client certificate, and channel
//! membership. What is extra is the control API: it registers a descriptor
//! saying what it does, then reports its health on a heartbeat, so an
//! administrator can see at a glance that the weather feed stopped an hour ago.

use core::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::newtype::string_newtype;
use crate::identity::{DeviceUid, ServiceId, ServiceName};

/// The longest a capability name may be.
pub const MAX_CAPABILITY_LENGTH: usize = 64;

/// Something a service says it can do.
///
/// Free-form on purpose: capabilities are how a sidecar advertises itself to
/// whatever might look for it, and pinning the vocabulary here would mean this
/// crate had to be changed before anybody could write a new kind of plugin.
/// The shape is constrained — a lower-case dotted token, like
/// `missions.publish` — so that capabilities can be compared, sorted and put in
/// a URL without anybody having to think about it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Capability(String);

impl Capability {
    /// Validates and normalises a capability name.
    pub fn parse(raw: &str) -> Result<Self, CapabilityError> {
        let normalised = raw.trim().to_lowercase();

        if normalised.is_empty() {
            return Err(CapabilityError::Empty);
        }

        let length = normalised.chars().count();
        if length > MAX_CAPABILITY_LENGTH {
            return Err(CapabilityError::TooLong {
                length,
                max: MAX_CAPABILITY_LENGTH,
            });
        }

        if let Some(character) = normalised.chars().find(|c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        }) {
            return Err(CapabilityError::IllegalCharacter { character });
        }

        let first = normalised.chars().next().unwrap_or_default();
        if !first.is_ascii_alphanumeric() {
            return Err(CapabilityError::MustStartAlphanumeric { character: first });
        }

        Ok(Self(normalised))
    }

    /// Wraps a name already known to be usable.
    pub fn from_storage(value: impl AsRef<str>) -> Self {
        Self(value.as_ref().to_lowercase())
    }
}

string_newtype!(Capability, CapabilityError, "a capability name");

/// The ways a capability name can be unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    /// The name was blank.
    Empty,

    /// The name was longer than [`MAX_CAPABILITY_LENGTH`].
    TooLong { length: usize, max: usize },

    /// The name held something other than a lower-case letter, a digit, or one
    /// of `.`, `_` and `-`.
    IllegalCharacter { character: char },

    /// The name began with punctuation.
    MustStartAlphanumeric { character: char },
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("A capability name cannot be blank."),
            Self::TooLong { length, max } => write!(
                f,
                "That capability name is {length} characters long, but the longest we accept is {max}."
            ),
            Self::IllegalCharacter { character } => write!(
                f,
                "A capability name cannot contain '{character}'. Use lower-case letters, digits, and any of . _ -"
            ),
            Self::MustStartAlphanumeric { character } => write!(
                f,
                "A capability name has to start with a letter or a digit, not '{character}'."
            ),
        }
    }
}

impl std::error::Error for CapabilityError {}

/// The addresses a service reaches this server on.
///
/// Reported rather than assigned: a sidecar in the same compose file reaches us
/// at a different address than one across a network, and knowing which is which
/// is what makes a misconfigured sidecar diagnosable from the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEndpoints {
    /// Where it connects for the CoT stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,

    /// Where it calls the Marti API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marti: Option<String>,

    /// Where it calls the service control API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
}

/// What a service says about itself when it registers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    pub name: ServiceName,

    /// What to call it in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,

    #[serde(default)]
    pub endpoints: ServiceEndpoints,
}

impl ServiceDescriptor {
    /// A descriptor for a service that has said nothing beyond its name.
    pub fn new(name: ServiceName) -> Self {
        Self {
            name,
            display_name: None,
            version: None,
            capabilities: Vec::new(),
            endpoints: ServiceEndpoints::default(),
        }
    }

    /// What to call this service in the UI.
    pub fn display(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.name.as_str())
    }

    /// The device identifier this service connects to the stream with.
    pub fn uid(&self) -> DeviceUid {
        self.name.uid()
    }
}

/// How a service says it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// It has registered but not yet said anything, or it stopped saying
    /// anything long enough ago that we no longer believe the last thing it
    /// said.
    #[default]
    Unknown,

    /// Doing its job.
    Healthy,

    /// Doing its job badly: retrying, falling behind, or running degraded.
    Degraded,

    /// Not doing its job.
    Unhealthy,
}

impl ServiceState {
    /// Every state, worst last.
    pub const ALL: &'static [Self] = &[
        Self::Unknown,
        Self::Healthy,
        Self::Degraded,
        Self::Unhealthy,
    ];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }

    /// A short phrase naming the state for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "Not reporting",
            Self::Healthy => "Healthy",
            Self::Degraded => "Degraded",
            Self::Unhealthy => "Unhealthy",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|state| state.as_str() == value)
    }

    /// Whether this state is worth drawing attention to in the UI.
    pub fn needs_attention(&self) -> bool {
        !matches!(self, Self::Healthy)
    }
}

/// What we currently believe about a service.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServiceStatus {
    #[serde(default)]
    pub state: ServiceState,

    /// What the service said about why, in its own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// When it last reported. Absent means it never has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

/// A service reporting in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub state: ServiceState,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Whatever numbers the service wants recorded: events handled, queue
    /// depth, time since its upstream last answered.
    ///
    /// Untyped because a plugin knows what is worth counting and this crate does
    /// not. It is rendered in the UI as-is, so nothing secret belongs in it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metrics: serde_json::Value,
}

impl Heartbeat {
    /// A service saying it is fine.
    pub fn healthy() -> Self {
        Self {
            state: ServiceState::Healthy,
            message: None,
            metrics: serde_json::Value::Null,
        }
    }
}

/// A registered service, as listed in the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSummary {
    pub id: ServiceId,

    #[serde(flatten)]
    pub descriptor: ServiceDescriptor,

    #[serde(default)]
    pub status: ServiceStatus,

    pub registered_at: DateTime<Utc>,

    /// The latest metrics the service reported.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metrics: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_descriptor() -> ServiceDescriptor {
        ServiceDescriptor {
            name: ServiceName::parse("weather-feed").unwrap(),
            display_name: Some("Weather feed".into()),
            version: Some("1.2.0".into()),
            capabilities: vec![
                Capability::parse("cot.publish").unwrap(),
                Capability::parse("missions.subscribe").unwrap(),
            ],
            endpoints: ServiceEndpoints {
                stream: Some("ssl://rustak:8089".into()),
                marti: Some("https://rustak:8443".into()),
                control: Some("https://rustak:8446".into()),
            },
        }
    }

    #[test]
    fn a_descriptor_round_trips_through_serde() {
        let descriptor = a_descriptor();
        let json = serde_json::to_string(&descriptor).unwrap();

        assert_eq!(
            serde_json::from_str::<ServiceDescriptor>(&json).unwrap(),
            descriptor
        );
        assert_eq!(descriptor.display(), "Weather feed");
        assert_eq!(descriptor.uid().as_str(), "SERVICE-weather-feed");
    }

    #[test]
    fn a_service_that_has_only_named_itself_still_registers() {
        let descriptor = ServiceDescriptor::new(ServiceName::parse("weather-feed").unwrap());

        assert_eq!(
            serde_json::to_string(&descriptor).unwrap(),
            r#"{"name":"weather-feed","endpoints":{}}"#
        );
        assert_eq!(descriptor.display(), "weather-feed");
        assert_eq!(
            serde_json::from_str::<ServiceDescriptor>(r#"{"name":"weather-feed"}"#).unwrap(),
            descriptor
        );
    }

    #[test]
    fn capabilities_are_normalised_and_bounded() {
        assert_eq!(
            Capability::parse(" COT.Publish ").unwrap().as_str(),
            "cot.publish"
        );

        assert_eq!(Capability::parse("  "), Err(CapabilityError::Empty));
        assert!(matches!(
            Capability::parse(&"a".repeat(MAX_CAPABILITY_LENGTH + 1)),
            Err(CapabilityError::TooLong { .. })
        ));
        assert!(matches!(
            Capability::parse("cot publish"),
            Err(CapabilityError::IllegalCharacter { character: ' ' })
        ));
        assert!(matches!(
            Capability::parse(".publish"),
            Err(CapabilityError::MustStartAlphanumeric { .. })
        ));

        let capability = Capability::parse("cot.publish").unwrap();
        let json = serde_json::to_string(&capability).unwrap();
        assert_eq!(json, "\"cot.publish\"");
        assert_eq!(
            serde_json::from_str::<Capability>(&json).unwrap(),
            capability
        );
        assert!(serde_json::from_str::<Capability>("\"cot publish\"").is_err());
    }

    #[test]
    fn a_heartbeat_round_trips_through_serde() {
        let heartbeat = Heartbeat {
            state: ServiceState::Degraded,
            message: Some("Upstream has not answered for 4 minutes.".into()),
            metrics: serde_json::json!({ "events_published": 1204, "queue_depth": 3 }),
        };

        let json = serde_json::to_string(&heartbeat).unwrap();
        assert_eq!(serde_json::from_str::<Heartbeat>(&json).unwrap(), heartbeat);

        // A service with nothing to report sends the smallest possible body.
        let healthy = Heartbeat::healthy();
        assert_eq!(
            serde_json::to_string(&healthy).unwrap(),
            r#"{"state":"healthy"}"#
        );
        assert_eq!(
            serde_json::from_str::<Heartbeat>(r#"{"state":"healthy"}"#).unwrap(),
            healthy
        );
    }

    #[test]
    fn a_summary_round_trips_through_serde() {
        let summary = ServiceSummary {
            id: ServiceId::new(1),
            descriptor: a_descriptor(),
            status: ServiceStatus {
                state: ServiceState::Healthy,
                message: None,
                last_heartbeat_at: Some("2026-09-18T13:00:00.250Z".parse().unwrap()),
            },
            registered_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            metrics: serde_json::json!({ "events_published": 1204 }),
        };

        let json = serde_json::to_string(&summary).unwrap();
        // The descriptor is flattened, so the UI reads a service's name from
        // the same place whether it came from a listing or from registration.
        assert!(json.contains(r#""name":"weather-feed""#));
        assert_eq!(
            serde_json::from_str::<ServiceSummary>(&json).unwrap(),
            summary
        );
    }

    #[test]
    fn a_service_that_has_never_reported_is_unknown() {
        let status = ServiceStatus::default();

        assert_eq!(status.state, ServiceState::Unknown);
        assert!(status.state.needs_attention());
        assert!(!ServiceState::Healthy.needs_attention());
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"state":"unknown"}"#
        );
    }

    #[test]
    fn states_round_trip_through_their_wire_form() {
        for state in ServiceState::ALL.iter().copied() {
            let json = serde_json::to_string(&state).unwrap();

            assert_eq!(json, format!("\"{}\"", state.as_str()));
            assert_eq!(serde_json::from_str::<ServiceState>(&json).unwrap(), state);
            assert_eq!(ServiceState::parse(state.as_str()), Some(state));
        }
    }
}
