//! The manual configuration package an administrator hands somebody who is
//! setting a client up by hand.
//!
//! An enrolling device fetches everything it needs over HTTPS; this is the
//! other path — a zip an operator sends out of band, which a person imports
//! into ATAK, WinTAK or iTAK to end up with the same stream configured.
//!
//! # It never mints a credential
//!
//! The request names an existing credential and an existing certificate, and
//! the server packages what is already there. Building one would put a
//! long-lived secret into a file whose only job is to be emailed around, and
//! nothing would ever revoke it.

use serde::{Deserialize, Serialize};

use crate::identity::{CredentialId, Username};

/// Which client the package is laid out for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigPackageVariant {
    /// ATAK and WinTAK: a Mission Package wrapped inside another Mission
    /// Package, because the outer one is what the import dialog accepts and the
    /// inner one is what carries the connection.
    #[default]
    WintakAtak,

    /// iTAK: a flat zip with no manifest at all, which is what its importer
    /// looks for.
    Itak,
}

impl ConfigPackageVariant {
    /// Every variant, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::WintakAtak, Self::Itak];

    /// The value carried on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WintakAtak => "wintak_atak",
            Self::Itak => "itak",
        }
    }

    /// A short phrase naming the variant for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::WintakAtak => "ATAK and WinTAK",
            Self::Itak => "iTAK",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|it| it.as_str() == value)
    }
}

/// What to build, and for whom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigPackageRequest {
    /// The account the package configures. It must already exist.
    pub username: Username,

    /// The client password the person will type into the enrolment prompt.
    ///
    /// Only meaningful when [`include_client_cert`](Self::include_client_cert)
    /// is `false`: the secret itself is never in the package (ATAK ignores a
    /// `username`/`password` pair in a `.pref` anyway), so this only asserts
    /// that a live credential exists to enrol with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<CredentialId>,

    #[serde(default)]
    pub variant: ConfigPackageVariant,

    /// Whether to put the account's own keystore in the package.
    ///
    /// `false` builds the enrolment variant, which carries only the truststore
    /// and asks the client to enrol for its own certificate on first connect —
    /// the safer default, because a keystore in a file is a credential that
    /// travels.
    #[serde(default)]
    pub include_client_cert: bool,
}

impl ConfigPackageRequest {
    /// A request for the enrolment variant, which carries no keystore.
    pub fn enrol(username: Username, variant: ConfigPackageVariant) -> Self {
        Self {
            username,
            credential_id: None,
            variant,
            include_client_cert: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variant_round_trips_through_its_wire_name() {
        for variant in ConfigPackageVariant::ALL {
            assert_eq!(
                ConfigPackageVariant::parse(variant.as_str()),
                Some(*variant)
            );

            let json = serde_json::to_string(variant).unwrap();
            assert_eq!(json, format!("\"{}\"", variant.as_str()));
        }

        assert_eq!(ConfigPackageVariant::parse("atak"), None);
    }

    #[test]
    fn a_request_round_trips() {
        let request = ConfigPackageRequest {
            username: Username::parse("ada").unwrap(),
            credential_id: Some(CredentialId::new(4)),
            variant: ConfigPackageVariant::Itak,
            include_client_cert: true,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<ConfigPackageRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn a_request_needs_nothing_but_a_username() {
        let request: ConfigPackageRequest =
            serde_json::from_value(serde_json::json!({ "username": "ada" })).unwrap();

        assert_eq!(request.variant, ConfigPackageVariant::WintakAtak);
        assert!(
            !request.include_client_cert,
            "a keystore is opt-in: it is a credential that travels",
        );
        assert_eq!(request.credential_id, None);
    }
}
