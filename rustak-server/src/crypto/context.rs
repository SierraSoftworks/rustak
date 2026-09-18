//! Identifies the row or key a sealed value belongs to.
//!
//! The rendered form is used as GCM additional authenticated data, which binds
//! each ciphertext to its location. Variants exist for every kind of secret
//! rustak stores so that the sealing and opening sites cannot drift apart: both
//! name the same variant rather than assembling a string independently.

use std::fmt;

use rustak_core::prelude::*;

/// Identifies the row a sealed value belongs to.
///
/// See the `crypto` module documentation for why this exists and why it is
/// never itself stored alongside the ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretContext<'a> {
    /// The private key of this installation's certificate authority.
    CaKey { certificate: CertificateId },

    /// The private key behind a server certificate, whether issued internally
    /// or obtained through ACME.
    ServerCertKey { certificate: CertificateId },

    /// The private key of a certificate bundle generated for a sidecar or a
    /// manually downloaded configuration package.
    ServiceCertKey { certificate: CertificateId },

    /// The account key an ACME account was registered with.
    AcmeAccount { account: i64 },

    /// The private key that signs our access and refresh tokens, identified by
    /// its `kid`.
    JwtSigningKey { kid: &'a str },

    /// The private key that signs mission package tokens, identified by its
    /// `kid`.
    MissionTokenKey { kid: &'a str },

    /// A refresh token an upstream identity provider issued us, identified by
    /// its `jti` rather than by its own value, so the context can be
    /// reconstructed without first decrypting the token it names. (M5)
    IdpRefreshToken { token: &'a str },

    /// A per-service configuration value an operator has marked secret. (M6)
    ServiceSecret {
        service: &'a ServiceName,
        key: &'a str,
    },
}

impl fmt::Display for SecretContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CaKey { certificate } => write!(f, "rustak/v1/ca-key/{certificate}"),
            Self::ServerCertKey { certificate } => write!(f, "rustak/v1/server-key/{certificate}"),
            Self::ServiceCertKey { certificate } => {
                write!(f, "rustak/v1/service-key/{certificate}")
            }
            Self::AcmeAccount { account } => write!(f, "rustak/v1/acme-account/{account}"),
            Self::JwtSigningKey { kid } => write!(f, "rustak/v1/jwt-key/{kid}"),
            Self::MissionTokenKey { kid } => write!(f, "rustak/v1/mission-key/{kid}"),
            Self::IdpRefreshToken { token } => write!(f, "rustak/v1/idp-refresh/{token}"),
            Self::ServiceSecret { service, key } => {
                write!(f, "rustak/v1/service-secret/{service}/{key}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_renders_the_documented_form() {
        assert_eq!(
            SecretContext::CaKey {
                certificate: CertificateId::new(7)
            }
            .to_string(),
            "rustak/v1/ca-key/7"
        );
        assert_eq!(
            SecretContext::ServerCertKey {
                certificate: CertificateId::new(7)
            }
            .to_string(),
            "rustak/v1/server-key/7"
        );
        assert_eq!(
            SecretContext::ServiceCertKey {
                certificate: CertificateId::new(7)
            }
            .to_string(),
            "rustak/v1/service-key/7"
        );
        assert_eq!(
            SecretContext::AcmeAccount { account: 42 }.to_string(),
            "rustak/v1/acme-account/42"
        );
        assert_eq!(
            SecretContext::JwtSigningKey { kid: "abc123" }.to_string(),
            "rustak/v1/jwt-key/abc123"
        );
        assert_eq!(
            SecretContext::MissionTokenKey { kid: "abc123" }.to_string(),
            "rustak/v1/mission-key/abc123"
        );
        assert_eq!(
            SecretContext::IdpRefreshToken { token: "jti-1" }.to_string(),
            "rustak/v1/idp-refresh/jti-1"
        );

        let service = ServiceName::parse("weather-feed").unwrap();
        assert_eq!(
            SecretContext::ServiceSecret {
                service: &service,
                key: "api_key"
            }
            .to_string(),
            "rustak/v1/service-secret/weather-feed/api_key"
        );
    }

    #[test]
    fn contexts_that_differ_only_by_id_render_differently() {
        // The property the whole design depends on: two rows of the same kind
        // must produce different additional authenticated data, or a
        // ciphertext relocated between them would still open.
        let first = SecretContext::CaKey {
            certificate: CertificateId::new(1),
        };
        let second = SecretContext::CaKey {
            certificate: CertificateId::new(2),
        };

        assert_ne!(first.to_string(), second.to_string());
        assert_ne!(first, second);
    }
}
