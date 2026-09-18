//! `[pki]` — the internal certificate authority and what it issues.
//!
//! rustak is its own CA. Every device that streams CoT or calls the Marti API
//! presents a certificate this CA issued, and the enrollment package hands the
//! device the CA to trust in return. That makes these settings unusually
//! sticky: a device keeps the certificate and the truststore it enrolled with,
//! so changing the CA means re-enrolling every device.
//!
//! # We choose the subject, not the client
//!
//! A device sends a certificate signing request containing a public key and a
//! subject it would like. We take the key and discard the subject, issuing
//! `CN=<username>` plus the name entries configured here. The alternative —
//! honouring what the CSR asked for — would let anybody who can enrol mint a
//! certificate naming somebody else, and the common name is what the stream and
//! Marti listeners resolve to a user.
//!
//! # Defaults are chosen for what TAK clients accept
//!
//! RSA-2048 rather than an elliptic curve, because the truststores in the TAK
//! ecosystem are conservative; PKCS#12 files written with the legacy algorithms
//! (3DES and a SHA-1 MAC) because that is what ATAK's keystore reads; the
//! well-known `atakatak` passphrase, because it is the one every TAK client
//! tries first and the file is handed over out of band anyway.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// What a redacted secret renders as in a `Debug` dump.
const REDACTED: &str = "<redacted>";

/// The relative distinguished name we never take from the configuration: the
/// common name is the username the certificate identifies.
const RESERVED_ENTRY: &str = "CN";

fn default_ca_common_name() -> String {
    "rustak CA".to_string()
}

fn default_organization() -> String {
    "rustak".to_string()
}

fn default_ca_validity() -> chrono::Duration {
    chrono::Duration::days(3650)
}

fn default_client_cert_validity() -> chrono::Duration {
    chrono::Duration::days(365)
}

/// 397 days: the longest a public certificate authority will issue for, kept
/// here so that an internal certificate and a public one expire on the same
/// rhythm.
fn default_server_cert_validity() -> chrono::Duration {
    chrono::Duration::days(397)
}

fn default_server_cert_renew_before() -> chrono::Duration {
    chrono::Duration::days(30)
}

/// The smallest RSA key we will accept in a certificate signing request.
fn default_csr_min_rsa_bits() -> u32 {
    2048
}

/// The passphrase every TAK client tries first on a PKCS#12 bundle.
fn default_p12_password() -> String {
    "atakatak".to_string()
}

fn default_true() -> bool {
    true
}

/// `[pki]`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PkiConfig {
    /// The common name of the root certificate authority.
    #[serde(default = "default_ca_common_name")]
    pub ca_common_name: String,

    /// The organisation (`O`) in every subject we issue, including the CA's.
    ///
    /// Ignored when `name_entries` is set, which replaces it wholesale.
    #[serde(default = "default_organization")]
    pub organization: String,

    /// The full, ordered list of relative distinguished names in every subject
    /// we issue, after the common name.
    ///
    /// `[["O", "rustak"], ["OU", "EUD"]]` issues `CN=<user>,O=rustak,OU=EUD`.
    /// Order matters: it is part of the subject's identity, and some TAK
    /// tooling compares subjects textually. `CN` is refused here because the
    /// common name is the username.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name_entries: Vec<(String, String)>,

    /// The key algorithm for the CA and for the server certificates it issues.
    /// Client keys are the client's own and are not affected.
    #[serde(default)]
    pub key_type: KeyType,

    /// How long the root CA certificate is valid.
    #[serde(
        default = "default_ca_validity",
        with = "rustak_core::config::duration::humane"
    )]
    pub ca_validity: chrono::Duration,

    /// How long an issued client certificate is valid. A device re-enrols when
    /// its certificate expires.
    #[serde(
        default = "default_client_cert_validity",
        with = "rustak_core::config::duration::humane"
    )]
    pub client_cert_validity: chrono::Duration,

    /// How long an internally issued server certificate is valid.
    #[serde(
        default = "default_server_cert_validity",
        with = "rustak_core::config::duration::humane"
    )]
    pub server_cert_validity: chrono::Duration,

    /// How long before expiry a server certificate is reissued.
    #[serde(
        default = "default_server_cert_renew_before",
        with = "rustak_core::config::duration::humane"
    )]
    pub server_cert_renew_before: chrono::Duration,

    /// Extra host names for the internal server certificate.
    ///
    /// `[server] domains` and `[acme] domains` are included automatically; this
    /// is for the names that are only reachable on the local network, such as
    /// `tak.lan`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_names: Vec<String>,

    /// IP addresses to put in the internal server certificate, for clients
    /// configured with an address rather than a name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_ips: Vec<IpAddr>,

    /// Whether a certificate must be one we have a record of, beyond chaining
    /// to our CA.
    ///
    /// On, so that a certificate deleted from the database stops working even
    /// if it has not expired and revocation has not propagated.
    #[serde(default = "default_true")]
    pub require_known_cert: bool,

    /// The smallest RSA key accepted in a certificate signing request.
    #[serde(default = "default_csr_min_rsa_bits")]
    pub csr_min_rsa_bits: u32,

    /// Whether an elliptic-curve signing request is accepted.
    #[serde(default = "default_true")]
    pub csr_allow_ecdsa: bool,

    /// The passphrase on the PKCS#12 bundles we produce for manual enrollment.
    #[serde(default = "default_p12_password")]
    pub p12_password: String,

    /// Whether PKCS#12 bundles are written with the legacy algorithms (3DES
    /// and a SHA-1 MAC) that ATAK's keystore reads.
    #[serde(default = "default_true")]
    pub p12_legacy: bool,

    /// Whether issued client certificates carry the TAK channels marker
    /// extended key usage OID.
    #[serde(default = "default_true")]
    pub channels_marker_eku: bool,
}

impl Default for PkiConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            ca_common_name: default_ca_common_name(),
            organization: default_organization(),
            name_entries: Vec::new(),
            key_type: KeyType::default(),
            ca_validity: default_ca_validity(),
            client_cert_validity: default_client_cert_validity(),
            server_cert_validity: default_server_cert_validity(),
            server_cert_renew_before: default_server_cert_renew_before(),
            server_names: Vec::new(),
            server_ips: Vec::new(),
            require_known_cert: true,
            csr_min_rsa_bits: default_csr_min_rsa_bits(),
            csr_allow_ecdsa: true,
            p12_password: default_p12_password(),
            p12_legacy: true,
            channels_marker_eku: true,
        }
    }
}

impl fmt::Debug for PkiConfig {
    /// Written out to keep `p12_password` out of logs and bug reports. It is
    /// usually the well-known TAK passphrase, but an installation is free to
    /// set a real one, and a `Debug` impl that leaks it only when it matters is
    /// worse than one that never does.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PkiConfig")
            .field("ca_common_name", &self.ca_common_name)
            .field("organization", &self.organization)
            .field("name_entries", &self.name_entries)
            .field("key_type", &self.key_type)
            .field("ca_validity", &self.ca_validity)
            .field("client_cert_validity", &self.client_cert_validity)
            .field("server_cert_validity", &self.server_cert_validity)
            .field("server_cert_renew_before", &self.server_cert_renew_before)
            .field("server_names", &self.server_names)
            .field("server_ips", &self.server_ips)
            .field("require_known_cert", &self.require_known_cert)
            .field("csr_min_rsa_bits", &self.csr_min_rsa_bits)
            .field("csr_allow_ecdsa", &self.csr_allow_ecdsa)
            .field("p12_password", &REDACTED)
            .field("p12_legacy", &self.p12_legacy)
            .field("channels_marker_eku", &self.channels_marker_eku)
            .finish()
    }
}

impl PkiConfig {
    /// The relative distinguished names that follow the common name in every
    /// subject we issue.
    ///
    /// `name_entries` when it is set, otherwise the single `O=<organization>`
    /// entry — and nothing at all when that is blank, because `O=` is not a
    /// subject component, it is an empty one.
    pub fn subject_entries(&self) -> Vec<(&str, &str)> {
        if !self.name_entries.is_empty() {
            return self
                .name_entries
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
        }

        if self.organization.is_empty() {
            return Vec::new();
        }

        vec![("O", self.organization.as_str())]
    }

    /// Reports a `name_entries` value we could not issue a certificate from.
    ///
    /// Called by [`Config::validate`](super::Config::validate); separate from
    /// the accessor so that the failure is reported when the file is loaded
    /// rather than when the first device tries to enrol.
    pub(super) fn malformed_name_entry(&self) -> Option<String> {
        self.name_entries.iter().find_map(|(key, value)| {
            let key = key.trim();
            let blank = key.is_empty() || value.trim().is_empty();

            (blank || key.eq_ignore_ascii_case(RESERVED_ENTRY))
                .then(|| format!("['{key}', '{value}']"))
        })
    }
}

/// The key algorithm used for the CA and the server certificates it issues.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyType {
    /// RSA, 2048 bits. The default: every TAK client accepts it.
    #[default]
    #[serde(rename = "rsa-2048")]
    Rsa2048,

    /// RSA, 3072 bits.
    #[serde(rename = "rsa-3072")]
    Rsa3072,

    /// ECDSA over P-256. Smaller and faster, but not every TAK truststore in
    /// the field accepts it.
    #[serde(rename = "ecdsa-p256")]
    EcdsaP256,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: PkiConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, PkiConfig::default());
        assert_eq!(parsed.ca_common_name, "rustak CA");
        assert_eq!(parsed.key_type, KeyType::Rsa2048);
        assert_eq!(parsed.ca_validity, chrono::Duration::days(3650));
        assert_eq!(parsed.client_cert_validity, chrono::Duration::days(365));
        assert_eq!(parsed.server_cert_validity, chrono::Duration::days(397));
        assert_eq!(parsed.csr_min_rsa_bits, 2048);
        assert!(parsed.require_known_cert);
        assert!(parsed.csr_allow_ecdsa);
        assert!(parsed.p12_legacy);
        assert!(parsed.channels_marker_eku);
    }

    #[test]
    fn the_default_subject_carries_the_organisation() {
        assert_eq!(
            PkiConfig::default().subject_entries(),
            vec![("O", "rustak")]
        );
    }

    #[test]
    fn name_entries_replace_the_organisation_and_keep_their_order() {
        // Order is part of the subject's identity, and some TAK tooling
        // compares subjects as text.
        let parsed: PkiConfig =
            toml::from_str(r#"name_entries = [["O", "Sierra"], ["OU", "EUD"]]"#).unwrap();

        assert_eq!(
            parsed.subject_entries(),
            vec![("O", "Sierra"), ("OU", "EUD")]
        );
    }

    #[test]
    fn a_blank_organisation_issues_a_bare_common_name() {
        let parsed: PkiConfig = toml::from_str(r#"organization = """#).unwrap();

        assert!(parsed.subject_entries().is_empty());
    }

    #[test]
    fn a_name_entry_cannot_take_over_the_common_name() {
        // The common name is the username the certificate identifies; letting
        // the configuration set a second one would make the subject ambiguous
        // to whatever reads it back.
        let parsed: PkiConfig =
            toml::from_str(r#"name_entries = [["CN", "somebody-else"]]"#).unwrap();

        assert!(parsed.malformed_name_entry().is_some());
    }

    #[test]
    fn an_empty_name_entry_is_reported() {
        let parsed: PkiConfig = toml::from_str(r#"name_entries = [["O", " "]]"#).unwrap();

        assert!(parsed.malformed_name_entry().is_some());
        assert!(PkiConfig::default().malformed_name_entry().is_none());
    }

    #[test]
    fn every_key_type_is_spelled_the_way_the_example_file_spells_it() {
        for (written, expected) in [
            ("rsa-2048", KeyType::Rsa2048),
            ("rsa-3072", KeyType::Rsa3072),
            ("ecdsa-p256", KeyType::EcdsaP256),
        ] {
            let parsed: PkiConfig = toml::from_str(&format!("key_type = \"{written}\"")).unwrap();

            assert_eq!(parsed.key_type, expected, "{written}");
        }
    }

    #[test]
    fn a_server_ip_is_validated_when_the_file_is_loaded() {
        let parsed: PkiConfig = toml::from_str(r#"server_ips = ["192.168.1.10", "::1"]"#).unwrap();
        assert_eq!(parsed.server_ips.len(), 2);

        let Err(err) = toml::from_str::<PkiConfig>(r#"server_ips = ["tak.example.com"]"#) else {
            panic!("a host name is not an IP address");
        };
        assert!(!err.to_string().is_empty(), "{err}");
    }

    #[test]
    fn the_pkcs12_passphrase_never_appears_in_a_debug_dump() {
        let config = PkiConfig {
            p12_password: "not-the-well-known-one".to_string(),
            ..PkiConfig::default()
        };

        let rendered = format!("{config:?}");

        assert!(!rendered.contains("not-the-well-known-one"), "{rendered}");
        assert!(rendered.contains(REDACTED), "{rendered}");
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<PkiConfig>(r#"ca_name = "rustak CA""#) else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("ca_name"), "{err}");
    }
}
