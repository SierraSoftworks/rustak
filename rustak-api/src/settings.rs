//! The installation-wide settings the first-run wizard writes and the admin UI
//! edits.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The settings that describe this installation to its clients.
///
/// Anything set in the configuration file wins over what is stored here: the
/// wizard exists so that a server can be brought up without hand-writing TOML,
/// not so that it can quietly disagree with a file an operator is managing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    /// What this server calls itself to clients, in the Marti version and
    /// configuration responses and at the top of the admin UI.
    pub name: String,

    /// The public host names clients reach this server on.
    ///
    /// The first is canonical: it is the host that goes into enrolment QR
    /// codes, preference files and certificate subject alternative names.
    #[serde(default)]
    pub domains: Vec<String>,

    /// The externally reachable base URL, when it cannot be derived from
    /// [`ServerSettings::domains`] and the listener's port — behind a proxy
    /// that terminates on a different port, for instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// This installation's stable identifier, minted once at first start.
    ///
    /// TAK clients and federated peers use it to tell one server from another
    /// when the host name changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,

    /// When the first-run wizard was finished. While this is absent the setup
    /// routes are open; once it is set they answer 410 forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_completed_at: Option<DateTime<Utc>>,
}

impl ServerSettings {
    /// The canonical host name clients should be told to use.
    pub fn canonical_domain(&self) -> Option<&str> {
        self.domains.first().map(String::as_str)
    }

    /// Whether the first-run wizard has been completed.
    pub fn is_setup_complete(&self) -> bool {
        self.setup_completed_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_serde() {
        let settings = ServerSettings {
            name: "rustak".into(),
            domains: vec!["tak.example.com".into(), "tak2.example.com".into()],
            base_url: Some("https://tak.example.com:8446".into()),
            node_id: Some("0d4f1a6e".into()),
            setup_completed_at: Some("2026-09-18T12:00:00.500Z".parse().unwrap()),
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerSettings>(&json).unwrap(),
            settings
        );

        assert_eq!(settings.canonical_domain(), Some("tak.example.com"));
        assert!(settings.is_setup_complete());
    }

    #[test]
    fn a_freshly_installed_server_has_only_a_name() {
        // This is what the UI sees between the first start and the wizard, so
        // every field the wizard fills in has to be optional.
        let settings: ServerSettings = serde_json::from_value(serde_json::json!({
            "name": "rustak",
        }))
        .unwrap();

        assert!(settings.domains.is_empty());
        assert_eq!(settings.canonical_domain(), None);
        assert!(!settings.is_setup_complete());
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"name":"rustak","domains":[]}"#
        );
    }
}
