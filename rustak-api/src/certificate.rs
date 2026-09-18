//! The certificates this server's authority has issued.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{CertificateId, Username};

/// What a certificate is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateKind {
    /// This installation's root certificate authority.
    Ca,

    /// A certificate one of our listeners presents.
    Server,

    /// A certificate an enrolled client authenticates with.
    Client,

    /// A certificate a sidecar authenticates with.
    Service,
}

impl CertificateKind {
    /// Every kind, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Ca, Self::Server, Self::Client, Self::Service];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ca => "ca",
            Self::Server => "server",
            Self::Client => "client",
            Self::Service => "service",
        }
    }

    /// A short phrase naming the kind for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ca => "Certificate authority",
            Self::Server => "Server",
            Self::Client => "Client",
            Self::Service => "Service",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }
}

/// How a certificate came to be issued.
///
/// Worth recording separately from the kind: revoking everything that came out
/// of one compromised enrolment token is a different question from revoking
/// every client certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateSource {
    /// A client sent a certificate signing request to the enrolment endpoint.
    #[default]
    Enrollment,

    /// An administrator asked the server to build a configuration package,
    /// so the key was generated here rather than on the device.
    AdminPackage,

    /// The server issued it to itself, for one of its own listeners or for its
    /// own authority.
    Internal,

    /// It came from a public authority through ACME.
    Acme,

    /// An operator supplied the certificate and key as files.
    Imported,
}

impl CertificateSource {
    /// Every source, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[
        Self::Enrollment,
        Self::AdminPackage,
        Self::Internal,
        Self::Acme,
        Self::Imported,
    ];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enrollment => "enrollment",
            Self::AdminPackage => "admin_package",
            Self::Internal => "internal",
            Self::Acme => "acme",
            Self::Imported => "imported",
        }
    }

    /// A short phrase naming the source for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Enrollment => "Enrolment",
            Self::AdminPackage => "Configuration package",
            Self::Internal => "Issued by this server",
            Self::Acme => "ACME",
            Self::Imported => "Imported",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.as_str() == value)
    }
}

/// A certificate as described to the admin UI.
///
/// Metadata only: the certificate itself is downloadable from its own endpoint
/// and the private key never leaves the server at all, except inside the
/// one-time package an administrator explicitly asks to build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Certificate {
    pub id: CertificateId,
    pub kind: CertificateKind,

    /// The serial number, as lower-case hexadecimal. Ours are 128-bit random
    /// values, so this does not fit in an integer.
    pub serial: String,

    /// The SHA-256 fingerprint of the DER encoding, as lower-case hexadecimal.
    /// This is what the client-certificate verifier looks up on every
    /// handshake, and what an administrator revokes by.
    pub fingerprint: String,

    /// The common name of the subject. For a client certificate this is the
    /// username, because we issue our own subject rather than trusting the one
    /// in the request.
    pub subject_cn: String,

    /// Who the certificate belongs to, when it belongs to somebody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<Username>,

    /// The subject alternative names, for a server certificate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub san: Vec<String>,

    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,

    /// When it was revoked. A revoked certificate fails the handshake and its
    /// live connections are dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub source: CertificateSource,
}

impl Certificate {
    /// Whether this certificate has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Whether this certificate is usable at `now`.
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        !self.is_revoked() && now >= self.not_before && now < self.not_after
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_certificate() -> Certificate {
        Certificate {
            id: CertificateId::new(1),
            kind: CertificateKind::Client,
            serial: "5b1f9c4e2a7d40318f6c0b9d3e8a1247".into(),
            fingerprint: "a".repeat(64),
            subject_cn: "alice".into(),
            username: Some(crate::identity::Username::parse("alice").unwrap()),
            san: Vec::new(),
            not_before: "2026-09-18T12:00:00.000Z".parse().unwrap(),
            not_after: "2027-09-18T12:00:00.000Z".parse().unwrap(),
            revoked_at: None,
            source: CertificateSource::Enrollment,
        }
    }

    #[test]
    fn a_certificate_round_trips_through_serde() {
        let certificate = a_certificate();
        let json = serde_json::to_string(&certificate).unwrap();

        assert_eq!(
            serde_json::from_str::<Certificate>(&json).unwrap(),
            certificate
        );
    }

    #[test]
    fn a_server_certificate_carries_its_alternative_names() {
        let certificate = Certificate {
            kind: CertificateKind::Server,
            username: None,
            san: vec!["tak.example.com".into(), "198.51.100.7".into()],
            source: CertificateSource::Acme,
            ..a_certificate()
        };

        let json = serde_json::to_string(&certificate).unwrap();
        assert_eq!(
            serde_json::from_str::<Certificate>(&json).unwrap(),
            certificate
        );
    }

    #[test]
    fn a_certificate_carries_no_key_material() {
        // The private key of a client certificate never exists here at all, and
        // the server's own keys are sealed at rest. This test exists so that
        // adding a field to carry one breaks a test named after the reason not
        // to.
        let serde_json::Value::Object(rendered) = serde_json::to_value(a_certificate()).unwrap()
        else {
            panic!("a certificate should serialise to an object");
        };

        let mut fields: Vec<&str> = rendered.keys().map(String::as_str).collect();
        fields.sort();

        assert_eq!(
            fields,
            vec![
                "fingerprint",
                "id",
                "kind",
                "not_after",
                "not_before",
                "serial",
                "source",
                "subject_cn",
                "username",
            ],
            "a field was added to the certificate DTO; check it carries no key material"
        );
    }

    #[test]
    fn validity_accounts_for_revocation_as_well_as_dates() {
        let certificate = a_certificate();
        let during: DateTime<Utc> = "2026-12-01T00:00:00Z".parse().unwrap();
        let after: DateTime<Utc> = "2028-01-01T00:00:00Z".parse().unwrap();
        let before: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();

        assert!(certificate.is_valid_at(during));
        assert!(!certificate.is_valid_at(after));
        assert!(!certificate.is_valid_at(before));

        let revoked = Certificate {
            revoked_at: Some("2026-10-01T00:00:00Z".parse().unwrap()),
            ..certificate
        };

        assert!(revoked.is_revoked());
        assert!(!revoked.is_valid_at(during));
    }

    #[test]
    fn kinds_and_sources_round_trip_through_their_wire_form() {
        for kind in CertificateKind::ALL.iter().copied() {
            let json = serde_json::to_string(&kind).unwrap();

            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(
                serde_json::from_str::<CertificateKind>(&json).unwrap(),
                kind
            );
            assert_eq!(CertificateKind::parse(kind.as_str()), Some(kind));
        }

        for source in CertificateSource::ALL.iter().copied() {
            let json = serde_json::to_string(&source).unwrap();

            assert_eq!(json, format!("\"{}\"", source.as_str()));
            assert_eq!(
                serde_json::from_str::<CertificateSource>(&json).unwrap(),
                source
            );
            assert_eq!(CertificateSource::parse(source.as_str()), Some(source));
        }
    }
}
