//! The client half of the `:8089` TLS handshake.
//!
//! Every rustak stream connection is mutually authenticated: the client
//! verifies the server against a pinned truststore, and the server identifies
//! the client by the certificate it presents (`compat/streaming.md` §1). There
//! is no anonymous path and no password path, so [`TlsIdentity`] carries all
//! three pieces of material or the connection does not happen.
//!
//! # The crypto provider is chosen explicitly
//!
//! `rustls` picks its cryptography backend from *crate features*, and panics
//! when it cannot tell which one was meant. rustak's dependency graph enables
//! both `aws-lc-rs` and `ring` (transitively, through crates we do not
//! control), so a `ClientConfig::builder()` here would panic at the first
//! connection rather than at compile time. [`provider`] therefore names one:
//! whatever the process has installed, or `aws-lc-rs`, which is what the rest
//! of rustak is built on.
//!
//! # Why the truststore is required
//!
//! A TAK deployment's server certificate is issued by the deployment's own CA,
//! which is exactly what enrolment hands the device along with its own
//! certificate. Falling back to the platform's public roots would mean trusting
//! every public CA to impersonate the TAK server, which is strictly worse than
//! refusing to start — so a missing truststore is a configuration error here,
//! not a default.

use std::path::Path;
use std::sync::Arc;

use rustls::ClientConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::{Endpoint, StreamError};

/// The certificates and key one client identity connects with.
pub struct TlsIdentity {
    /// The certificates the server's chain is verified against.
    truststore: Vec<CertificateDer<'static>>,

    /// The client certificate chain, leaf first.
    cert_chain: Vec<CertificateDer<'static>>,

    /// The private key belonging to the leaf of `cert_chain`.
    key: PrivateKeyDer<'static>,
}

impl TlsIdentity {
    /// Loads a truststore, a client certificate chain and its key from PEM.
    ///
    /// The certificate file may hold a chain; every certificate in it is sent,
    /// leaf first, in file order. The key may be PKCS#8, PKCS#1 or SEC1.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Identity`] naming the file that could not be
    /// read, held no PEM of the kind expected, or held a key we cannot use.
    pub fn from_pem_files(
        truststore: impl AsRef<Path>,
        certificate: impl AsRef<Path>,
        key: impl AsRef<Path>,
    ) -> Result<Self, StreamError> {
        let truststore = certificates(truststore.as_ref(), "truststore")?;
        let cert_chain = certificates(certificate.as_ref(), "client certificate")?;
        let key_path = key.as_ref();
        let key = PrivateKeyDer::from_pem_file(key_path)
            .map_err(|error| pem_error(key_path, "private key", &error))?;

        Ok(Self {
            truststore,
            cert_chain,
            key,
        })
    }

    /// Loads the material a sidecar's `[service]` section names.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Identity`] when the identity has no certificate,
    /// no key or no truststore — the stream needs all three — or when any of
    /// the three files cannot be loaded.
    pub fn from_identity(
        identity: &rustak_core::service::ServiceIdentity,
    ) -> Result<Self, StreamError> {
        let (Some(certificate), Some(key), Some(truststore)) = (
            identity.certificate(),
            identity.key(),
            identity.truststore(),
        ) else {
            return Err(StreamError::Identity(format!(
                "the '{}' service has no certificate, key and truststore, and a TAK stream needs all three",
                identity.name().as_str(),
            )));
        };

        Self::from_pem_files(truststore, certificate, key)
    }

    /// How many certificates the truststore holds.
    #[must_use]
    pub fn roots(&self) -> usize {
        self.truststore.len()
    }

    /// Builds the `rustls` configuration this identity connects with.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Identity`] when a root will not parse, or when
    /// the key does not belong to the certificate.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>, StreamError> {
        let mut roots = rustls::RootCertStore::empty();
        for root in &self.truststore {
            roots.add(root.clone()).map_err(|error| {
                StreamError::Identity(format!("a truststore certificate is unusable ({error})"))
            })?;
        }

        let config = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|error| {
                StreamError::Identity(format!("the TLS provider is unusable ({error})"))
            })?
            .with_root_certificates(roots)
            .with_client_auth_cert(self.cert_chain.clone(), self.key.clone_key())
            .map_err(|error| {
                StreamError::Identity(format!(
                    "the client certificate and key do not match ({error})"
                ))
            })?;

        Ok(Arc::new(config))
    }
}

impl std::fmt::Debug for TlsIdentity {
    /// Counts rather than contents: a `Debug` dump of a configuration must not
    /// be a way to exfiltrate a private key (`conventions.md`).
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TlsIdentity")
            .field("roots", &self.truststore.len())
            .field("chain", &self.cert_chain.len())
            .field("key", &"***")
            .finish()
    }
}

/// The cryptography backend TLS connections are built on.
///
/// Prefers whatever the process installed — an application that called
/// [`CryptoProvider::install_default`](rustls::crypto::CryptoProvider::install_default)
/// meant it — and otherwise `aws-lc-rs`, the backend the rest of rustak uses.
/// Naming one is not optional: with more than one backend feature enabled,
/// `rustls`' own automatic choice panics.
#[must_use]
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// Dials `endpoint` and completes the TLS handshake against it.
///
/// # Errors
///
/// Returns [`StreamError::Io`] when the socket or the handshake fails, and
/// [`StreamError::Endpoint`] when the host is not a name a certificate can be
/// checked against.
pub async fn connect(
    endpoint: &Endpoint,
    config: Arc<ClientConfig>,
) -> Result<TlsStream<TcpStream>, StreamError> {
    let name = ServerName::try_from(endpoint.host.clone()).map_err(|_| {
        StreamError::Endpoint(format!(
            "{:?}, whose host is neither a DNS name nor an IP address",
            endpoint.host,
        ))
    })?;

    let tcp = dial(endpoint).await?;

    Ok(TlsConnector::from(config).connect(name, tcp).await?)
}

/// Opens the TCP socket underneath a stream connection.
pub(crate) async fn dial(endpoint: &Endpoint) -> Result<TcpStream, StreamError> {
    let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await?;

    // CoT messages are small and latency-sensitive; Nagle would hold a position
    // report back waiting for a second one that may be seconds away.
    tcp.set_nodelay(true)?;

    Ok(tcp)
}

/// Reads every certificate in a PEM file.
fn certificates(path: &Path, what: &str) -> Result<Vec<CertificateDer<'static>>, StreamError> {
    let certificates = CertificateDer::pem_file_iter(path)
        .map_err(|error| pem_error(path, what, &error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| pem_error(path, what, &error))?;

    match certificates.is_empty() {
        true => Err(StreamError::Identity(format!(
            "{} holds no certificates",
            path.display(),
        ))),
        false => Ok(certificates),
    }
}

/// Names the file and what we wanted from it, which is the part of a PEM
/// failure an operator can act on.
fn pem_error(path: &Path, what: &str, error: &rustls_pki_types::pem::Error) -> StreamError {
    StreamError::Identity(format!(
        "the {what} at {} could not be read ({error})",
        path.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_reported_with_its_path_and_its_purpose() {
        // "No such file or directory (os error 2)" on its own has sent more
        // than one operator to the wrong file.
        let error = certificates(Path::new("/nonexistent/truststore.pem"), "truststore")
            .expect_err("a missing file should not load");

        assert!(error.to_string().contains("truststore"), "{error}");
        assert!(
            error.to_string().contains("/nonexistent/truststore.pem"),
            "{error}"
        );
    }

    #[test]
    fn an_identity_without_all_three_files_is_refused_before_the_handshake() {
        use rustak_core::identity::ServiceName;
        use rustak_core::service::ServiceIdentity;

        let identity = ServiceIdentity::new(ServiceName::parse("adsb-feed").unwrap())
            .with_client_cert("/c.pem", "/k.pem");

        let error = TlsIdentity::from_identity(&identity).expect_err("no truststore, no stream");

        assert!(error.to_string().contains("adsb-feed"), "{error}");
        assert!(error.to_string().contains("truststore"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_pem_at_all_is_a_configuration_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cert.pem");
        std::fs::write(&path, b"this is not a certificate\n").unwrap();

        let error = certificates(&path, "client certificate").expect_err("junk should not load");

        assert!(matches!(error, StreamError::Identity(_)), "{error:?}");
    }

    #[test]
    fn a_provider_is_always_available_without_installing_one() {
        // The regression this guards: `ClientConfig::builder()` panics in this
        // dependency graph, because both backend features are enabled
        // somewhere in it. A client that only fails at connect time would have
        // shipped.
        assert!(!provider().cipher_suites.is_empty());
    }

    #[test]
    fn a_debug_dump_of_an_identity_never_carries_key_material() {
        let identity = TlsIdentity {
            truststore: vec![CertificateDer::from(vec![1, 2, 3])],
            cert_chain: vec![CertificateDer::from(vec![4, 5, 6])],
            key: PrivateKeyDer::Pkcs8(rustls_pki_types::PrivatePkcs8KeyDer::from(vec![7, 8, 9])),
        };

        let printed = format!("{identity:?}");

        assert!(printed.contains("roots: 1"), "{printed}");
        assert!(!printed.contains('7'), "{printed}");
        assert_eq!(identity.roots(), 1);
    }
}
