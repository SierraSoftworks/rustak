//! The first-run wizard.
//!
//! A freshly installed server has no administrator, no certificate authority
//! and no idea what host name it will be reached on. The wizard fills those in
//! from the browser so that bringing a server up does not mean hand-writing
//! TOML — but it is a one-way door: once
//! [`SetupStatus::setup_completed`] is set, every route below answers `410`
//! forever, so the wizard cannot be used to take over a running installation.
//!
//! The first step is gated by a one-time setup token, which the server writes
//! to a file and logs at first start. Whoever can read that has already got the
//! server's filesystem, so it is a proof of possession rather than a secret in
//! its own right.

use core::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::Username;

/// How far through the wizard this installation is.
///
/// Public, because the UI has to ask before anybody can sign in. It says which
/// steps remain and nothing else — not the setup token, not who the
/// administrator is, not what the host name would be.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SetupStatus {
    /// Whether the UI should send somebody to the wizard rather than to the
    /// login page.
    pub needs_setup: bool,

    /// Whether an administrator exists.
    #[serde(default)]
    pub has_admin: bool,

    /// Whether the internal certificate authority has been created.
    #[serde(default)]
    pub has_ca: bool,

    /// Whether the server knows what host name it is reached on.
    #[serde(default)]
    pub has_server_name: bool,

    /// Whether the wizard has been finished, after which its routes are gone.
    #[serde(default)]
    pub setup_completed: bool,

    /// The server's version, so the wizard can show what it is setting up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Creates the first administrator.
///
/// No password: the response hands back a short-lived registration token, and
/// the wizard's next step uses it to register the administrator's first
/// passkey.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAdminRequest {
    /// The one-time token the server wrote out at first start.
    pub setup_token: String,

    pub username: Username,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl fmt::Debug for CreateAdminRequest {
    /// Redacts the setup token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateAdminRequest")
            .field("setup_token", &"***")
            .field("username", &self.username)
            .field("display_name", &self.display_name)
            .field("email", &self.email)
            .finish()
    }
}

/// What creating the first administrator returns.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminCreated {
    pub username: Username,

    /// A short-lived token authorising exactly one thing: registering this
    /// administrator's first passkey. They cannot sign in until they have.
    pub registration_token: String,

    /// How long that token lasts, in seconds.
    pub expires_in: u64,
}

impl fmt::Debug for AdminCreated {
    /// Redacts the registration token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminCreated")
            .field("username", &self.username)
            .field("registration_token", &"***")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// The key algorithm for the internal certificate authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaKeyType {
    /// The safe default. Older TAK clients and their Java keystores are
    /// reliable with RSA and less so with anything else.
    #[default]
    Rsa2048,

    /// Smaller and faster, for an installation that knows every client it will
    /// ever serve.
    EcdsaP256,
}

impl CaKeyType {
    /// Every key type, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Rsa2048, Self::EcdsaP256];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rsa2048 => "rsa2048",
            Self::EcdsaP256 => "ecdsa_p256",
        }
    }

    /// A short phrase naming the key type for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rsa2048 => "RSA 2048 (most compatible)",
            Self::EcdsaP256 => "ECDSA P-256",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }
}

/// Creates the internal certificate authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitCaRequest {
    /// The authority's common name, which is what clients will show when they
    /// are asked to trust it.
    pub common_name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,

    #[serde(default)]
    pub key_type: CaKeyType,
}

/// The certificate authority, once it exists.
///
/// Its private key is sealed at rest and never leaves the server, so there is
/// nothing here but what a client would see anyway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaSummary {
    /// The authority's distinguished name.
    pub subject: String,

    /// The SHA-256 fingerprint of the DER encoding, as lower-case hexadecimal.
    /// This is what somebody compares by hand when trusting the authority on a
    /// device.
    pub fingerprint: String,

    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

/// Tells the server what it is called and where it is reachable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettingsRequest {
    pub name: String,

    /// The public host names, canonical first. These become the subject
    /// alternative names of the server certificate and the host in every
    /// enrolment QR code.
    #[serde(default)]
    pub domains: Vec<String>,

    /// The externally reachable base URL, when it cannot be derived from the
    /// host name and the listener's port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_installation_needs_every_step() {
        let status = SetupStatus {
            needs_setup: true,
            ..SetupStatus::default()
        };

        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<SetupStatus>(&json).unwrap(), status);
        assert!(!status.has_admin);
        assert!(!status.setup_completed);
    }

    #[test]
    fn a_finished_installation_round_trips_through_serde() {
        let status = SetupStatus {
            needs_setup: false,
            has_admin: true,
            has_ca: true,
            has_server_name: true,
            setup_completed: true,
            version: Some("0.1.0".into()),
        };

        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<SetupStatus>(&json).unwrap(), status);
    }

    #[test]
    fn the_bootstrap_secrets_are_redacted_in_debug_output() {
        let request = CreateAdminRequest {
            setup_token: "setup-secret".into(),
            username: Username::parse("alice").unwrap(),
            display_name: Some("Alice Smith".into()),
            email: None,
        };

        let debug = format!("{request:?}");
        assert!(!debug.contains("setup-secret"), "the token leaked: {debug}");
        assert!(debug.contains("alice"));

        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<CreateAdminRequest>(&json).unwrap(),
            request
        );

        let created = AdminCreated {
            username: Username::parse("alice").unwrap(),
            registration_token: "registration-secret".into(),
            expires_in: 600,
        };

        let debug = format!("{created:?}");
        assert!(
            !debug.contains("registration-secret"),
            "the token leaked: {debug}"
        );

        let json = serde_json::to_string(&created).unwrap();
        assert_eq!(
            serde_json::from_str::<AdminCreated>(&json).unwrap(),
            created
        );
    }

    #[test]
    fn there_is_no_password_in_the_wizard() {
        // The wizard registers a passkey; a request carrying a password is a
        // request written against the design this one replaced.
        let request: CreateAdminRequest = serde_json::from_value(serde_json::json!({
            "setup_token": "setup-secret",
            "username": "alice",
            "password": "hunter2",
        }))
        .unwrap();

        let rendered = serde_json::to_value(&request).unwrap();
        assert!(
            rendered.get("password").is_none(),
            "a password must not survive into the request the server acts on"
        );
    }

    #[test]
    fn the_certificate_authority_request_round_trips_through_serde() {
        let request = InitCaRequest {
            common_name: "rustak Root CA".into(),
            organization: Some("Example".into()),
            key_type: CaKeyType::EcdsaP256,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<InitCaRequest>(&json).unwrap(),
            request
        );

        // The compatible default applies when the wizard does not ask.
        let defaulted: InitCaRequest = serde_json::from_value(serde_json::json!({
            "common_name": "rustak Root CA",
        }))
        .unwrap();
        assert_eq!(defaulted.key_type, CaKeyType::Rsa2048);

        for key_type in CaKeyType::ALL.iter().copied() {
            let json = serde_json::to_string(&key_type).unwrap();

            assert_eq!(json, format!("\"{}\"", key_type.as_str()));
            assert_eq!(serde_json::from_str::<CaKeyType>(&json).unwrap(), key_type);
            assert_eq!(CaKeyType::parse(key_type.as_str()), Some(key_type));
        }
    }

    #[test]
    fn the_authority_summary_and_settings_request_round_trip() {
        let summary = CaSummary {
            subject: "CN=rustak Root CA,O=Example".into(),
            fingerprint: "b".repeat(64),
            not_before: "2026-09-18T12:00:00Z".parse().unwrap(),
            not_after: "2036-09-18T12:00:00Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert_eq!(serde_json::from_str::<CaSummary>(&json).unwrap(), summary);

        let settings = ServerSettingsRequest {
            name: "rustak".into(),
            domains: vec!["tak.example.com".into()],
            base_url: None,
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(json, r#"{"name":"rustak","domains":["tak.example.com"]}"#);
        assert_eq!(
            serde_json::from_str::<ServerSettingsRequest>(&json).unwrap(),
            settings
        );
    }
}
