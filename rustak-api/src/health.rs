//! What the server says about itself when asked whether it is working.

use serde::{Deserialize, Serialize};

/// Whether a part of the server is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    /// Working as far as we can tell.
    #[default]
    Ok,

    /// Working, but not well: slow, retrying, or running on a fallback.
    Degraded,

    /// Not working.
    Down,
}

impl ComponentStatus {
    /// Every status, worst last.
    pub const ALL: &'static [Self] = &[Self::Ok, Self::Degraded, Self::Down];

    /// The value carried on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }

    /// A short phrase naming the status for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ok => "Healthy",
            Self::Degraded => "Degraded",
            Self::Down => "Unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ok" => Self::Ok,
            "degraded" => Self::Degraded,
            "down" => Self::Down,
            _ => return None,
        })
    }

    /// Whether the server should still be sent traffic.
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Ok | Self::Degraded)
    }
}

/// The health check, which is public.
///
/// Deliberately thin: it is reachable without authentication, so it says
/// whether the server is working and which version it is, and nothing that
/// would help someone decide how to attack it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// The worst of the component statuses.
    pub status: ComponentStatus,

    pub version: String,

    /// How long this process has been running.
    pub uptime_seconds: u64,

    /// Whether a trivial query against a reader connection succeeded.
    pub database: ComponentStatus,

    /// A short note about why the status is not `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Health {
    /// A healthy server.
    pub fn ok(version: impl Into<String>, uptime_seconds: u64) -> Self {
        Self {
            status: ComponentStatus::Ok,
            version: version.into(),
            uptime_seconds,
            database: ComponentStatus::Ok,
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_round_trips_through_serde() {
        let health = Health {
            status: ComponentStatus::Degraded,
            version: "0.1.0".into(),
            uptime_seconds: 1234,
            database: ComponentStatus::Degraded,
            message: Some("The database is answering slowly.".into()),
        };

        let json = serde_json::to_string(&health).unwrap();
        assert_eq!(serde_json::from_str::<Health>(&json).unwrap(), health);
    }

    #[test]
    fn a_healthy_server_omits_the_message() {
        let health = Health::ok("0.1.0", 5);

        assert_eq!(
            serde_json::to_string(&health).unwrap(),
            r#"{"status":"ok","version":"0.1.0","uptime_seconds":5,"database":"ok"}"#
        );
    }

    #[test]
    fn statuses_round_trip_through_their_wire_form() {
        for status in ComponentStatus::ALL.iter().copied() {
            let json = serde_json::to_string(&status).unwrap();

            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(
                serde_json::from_str::<ComponentStatus>(&json).unwrap(),
                status
            );
            assert_eq!(ComponentStatus::parse(status.as_str()), Some(status));
        }

        assert!(ComponentStatus::Degraded.is_serving());
        assert!(!ComponentStatus::Down.is_serving());
        assert_eq!(ComponentStatus::parse("fine"), None);
    }
}
