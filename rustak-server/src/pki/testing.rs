//! A throwaway certificate authority for tests.
//!
//! Deliberately built from the real pieces — [`load_or_create_root_ca`],
//! [`parse_csr`] and [`issue_client_cert`] — rather than from a hand-rolled
//! rcgen certificate, so that a handshake test proves the certificates rustak
//! actually issues are the ones rustls actually accepts. A test authority that
//! took a shortcut would be a test that passed while enrolment was broken.
//!
//! Elliptic curve throughout, because an RSA key is hundreds of milliseconds of
//! keygen and every test here wants several.
//!
//! [`load_or_create_root_ca`]: crate::pki::ca::load_or_create_root_ca
//! [`parse_csr`]: crate::pki::csr::parse_csr
//! [`issue_client_cert`]: crate::pki::issue::issue_client_cert

use rustak_core::prelude::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::ca::{CaMaterial, load_or_create_root_ca};
use super::csr::parse_csr;
use super::issue::{IssueRequest, issue_client_cert};
use super::keys::{KeyType, generate_key};
use crate::config::PkiConfig;
use crate::crypto::SecretStore;
use crate::db::Database;

/// The name entries a test authority issues with.
pub const TEST_NAME_ENTRIES: &[(&str, &str)] = &[("O", "rustak"), ("OU", "test")];

/// A certificate authority that exists for the length of one test.
pub struct TestAuthority {
    ca: CaMaterial,
    /// Holds the temporary `ca.crt` export alive; dropping it removes the file.
    _data_dir: tempfile::TempDir,
}

impl TestAuthority {
    /// Creates an authority, exactly as a first start-up would.
    pub async fn new() -> Self {
        let data_dir = tempfile::tempdir().expect("a temporary directory");
        let config = PkiConfig {
            key_type: KeyType::EcdsaP256,
            ..PkiConfig::default()
        };

        let ca = load_or_create_root_ca(
            &Database::open_in_memory()
                .await
                .expect("an in-memory database"),
            &SecretStore::ephemeral(),
            &config,
            data_dir.path(),
        )
        .await
        .expect("a certificate authority");

        Self {
            ca,
            _data_dir: data_dir,
        }
    }

    /// The signing material, for anything that wants the real type.
    pub fn material(&self) -> &CaMaterial {
        &self.ca
    }

    /// The authority's own certificate, which clients trust and listeners
    /// verify against.
    pub fn certificate(&self) -> CertificateDer<'static> {
        self.ca.certificate().clone()
    }

    /// Enrols a client the long way round: key, signing request, policy-free
    /// issuance.
    pub fn issue(&self, username: &str) -> TestClient {
        self.issue_with(username, |request| request)
    }

    /// Enrols a client, adjusting the issuance request first — used for the
    /// cases that need an unusual validity window.
    pub fn issue_with(
        &self,
        username: &str,
        adjust: impl for<'a> FnOnce(IssueRequest<'a>) -> IssueRequest<'a>,
    ) -> TestClient {
        let key = generate_key(KeyType::EcdsaP256).expect("a key");
        let mut params = rcgen::CertificateParams::default();

        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, username);

        let csr = parse_csr(
            params
                .serialize_request(&key)
                .expect("a signing request")
                .der(),
        )
        .expect("our own signing request parses");

        let user = Username::parse(username).expect("a usable username");
        let request = adjust(IssueRequest {
            username: &user,
            client_uid: None,
            validity: chrono::Duration::days(365),
            not_before_skew: chrono::Duration::minutes(5),
            channels_marker: false,
        });

        let issued = issue_client_cert(&self.ca, &csr, TEST_NAME_ENTRIES, &request)
            .expect("the authority issues");

        TestClient {
            der: issued.der,
            fingerprint: issued.fingerprint,
            serial_hex: issued.serial_hex,
            key_pkcs8: key.serialize_der(),
        }
    }
}

impl std::fmt::Debug for TestAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestAuthority")
            .field("subject", &self.ca.subject())
            .finish_non_exhaustive()
    }
}

/// An enrolled client: what it presents, and what it signs with.
pub struct TestClient {
    pub der: CertificateDer<'static>,
    pub fingerprint: String,
    /// Exactly what issuance recorded, so a test can hold the handshake's
    /// reading of the same certificate against it.
    pub serial_hex: String,
    pub key_pkcs8: Vec<u8>,
}

impl TestClient {
    /// The key as rustls wants it for a client configuration.
    pub fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key_pkcs8.clone().into())
    }
}

impl std::fmt::Debug for TestClient {
    /// Written out so a failing assertion does not print a private key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestClient")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_test_authority_issues_certificates_that_chain_to_it() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");

        let leaf = x509_parser::parse_x509_certificate(&client.der).unwrap().1;
        let authority_der = authority.certificate();
        let root = x509_parser::parse_x509_certificate(&authority_der)
            .unwrap()
            .1;

        assert_eq!(leaf.subject().to_string(), "CN=alice, O=rustak, OU=test");
        leaf.verify_signature(Some(root.public_key())).unwrap();
        assert_eq!(client.fingerprint.len(), 64);
    }

    #[tokio::test]
    async fn two_clients_get_two_certificates() {
        let authority = TestAuthority::new().await;

        assert_ne!(
            authority.issue("alice").fingerprint,
            authority.issue("bob").fingerprint
        );
    }

    #[tokio::test]
    async fn a_client_never_renders_its_key() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");

        assert!(!format!("{client:?}").contains(&hex::encode(&client.key_pkcs8)));
        assert!(format!("{authority:?}").contains("rustak CA"));
        assert!(matches!(client.private_key(), PrivateKeyDer::Pkcs8(_)));
    }
}
