//! A sidecar's own identity, as the sidecar itself holds it.
//!
//! `rustak-api`'s `ServiceDescriptor` is what a sidecar *tells the server about
//! itself* — its name, version, capabilities and endpoints — and travels as
//! JSON. [`ServiceIdentity`] is the other half: the material the sidecar needs
//! to be that service, which never leaves the process. Its control-API token,
//! and the paths to the certificate, key and truststore it connects to the CoT
//! stream with.
//!
//! Keeping them apart is the point. A sidecar loads a [`ServiceIdentity`] from
//! its configuration and hands the *descriptor* to the server; nothing that
//! would let another process impersonate it is ever part of what it publishes.
//!
//! # A sidecar is an EUD
//!
//! There is no separate protocol for plugins. A sidecar connects to `:8089` with
//! a client certificate like any ATAK device, under a user of kind `service`,
//! with its own group scoping — which is what makes an ADS-B feed's traffic
//! routable, filterable and auditable by exactly the same rules as a person's.
//! Its `clientUid` is derived from its name ([`ServiceName::uid`]) so the two
//! cannot drift apart.

use std::path::{Path, PathBuf};

pub use rustak_api::service::*;

use crate::identity::{DeviceUid, Secret, ServiceName};

/// Everything a sidecar needs in order to be itself.
///
/// ```
/// # use rustak_core::identity::ServiceName;
/// # use rustak_core::service::ServiceIdentity;
/// let identity = ServiceIdentity::new(ServiceName::parse("adsb-feed").unwrap());
///
/// // The stream uid follows the name, so a rename cannot leave a stale device
/// // behind on the server.
/// assert_eq!(identity.uid().as_str(), "SERVICE-adsb-feed");
///
/// // Nothing secret appears in a log line.
/// assert!(!format!("{identity:?}").contains("token"));
/// ```
#[derive(Clone)]
pub struct ServiceIdentity {
    name: ServiceName,
    credential: Option<Secret>,
    certificate: Option<PathBuf>,
    key: Option<PathBuf>,
    truststore: Option<PathBuf>,
}

impl ServiceIdentity {
    /// A named identity with no credentials attached yet.
    ///
    /// The name is required rather than optional because everything else here
    /// is derived from or scoped to it — the uid, the certificate subject, the
    /// control-API path — so an unnamed identity would be a value with nothing
    /// to do but fail later.
    pub fn new(name: ServiceName) -> Self {
        Self {
            name,
            credential: None,
            certificate: None,
            key: None,
            truststore: None,
        }
    }

    /// The service's name.
    pub fn name(&self) -> &ServiceName {
        &self.name
    }

    /// The `clientUid` this service connects to the CoT stream with.
    pub fn uid(&self) -> DeviceUid {
        self.name.uid()
    }

    /// The token this service authenticates to `/api/v1/services/*` with.
    ///
    /// Its *primary* identity is the client certificate; this is for the
    /// control API, which a sidecar may use before it has a certificate.
    pub fn credential(&self) -> Option<&Secret> {
        self.credential.as_ref()
    }

    /// Attaches the control-API token.
    #[must_use]
    pub fn with_credential(mut self, credential: Secret) -> Self {
        self.credential = Some(credential);
        self
    }

    /// Attaches the client certificate and its private key.
    #[must_use]
    pub fn with_client_cert(
        mut self,
        certificate: impl Into<PathBuf>,
        key: impl Into<PathBuf>,
    ) -> Self {
        self.certificate = Some(certificate.into());
        self.key = Some(key.into());
        self
    }

    /// Attaches the truststore used to verify the server's certificate.
    #[must_use]
    pub fn with_truststore(mut self, truststore: impl Into<PathBuf>) -> Self {
        self.truststore = Some(truststore.into());
        self
    }

    /// The client certificate's path, if one was configured.
    pub fn certificate(&self) -> Option<&Path> {
        self.certificate.as_deref()
    }

    /// The private key's path, if one was configured.
    pub fn key(&self) -> Option<&Path> {
        self.key.as_deref()
    }

    /// The truststore's path, if one was configured.
    pub fn truststore(&self) -> Option<&Path> {
        self.truststore.as_deref()
    }

    /// Whether this identity can open a mutually authenticated stream
    /// connection, which needs both halves of the client certificate.
    pub fn has_client_cert(&self) -> bool {
        self.certificate.is_some() && self.key.is_some()
    }
}

impl From<&ServiceIdentity> for ServiceDescriptor {
    /// The public half of an identity: its name, and nothing else.
    ///
    /// This is the boundary the module documentation describes. A descriptor is
    /// published — to the server on registration, and from there to the admin
    /// UI — so it is built by *naming* the few fields that may cross rather than
    /// by copying the identity and removing the ones that may not. A field added
    /// to [`ServiceIdentity`] therefore stays private until somebody decides
    /// otherwise here.
    ///
    /// The descriptive fields a sidecar reports — version, capabilities,
    /// endpoints — are not part of its identity, so the caller fills those in on
    /// the descriptor this returns.
    fn from(identity: &ServiceIdentity) -> Self {
        Self::new(identity.name().clone())
    }
}

impl std::fmt::Debug for ServiceIdentity {
    /// Names the paths, never the token: a sidecar that logs its own identity
    /// at start-up is a normal thing to do, and must stay a safe one.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceIdentity")
            .field("name", &self.name)
            .field("credential", &self.credential)
            .field("certificate", &self.certificate)
            .field("key", &self.key)
            .field("truststore", &self.truststore)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ServiceIdentity {
        ServiceIdentity::new(ServiceName::parse("adsb-feed").unwrap())
    }

    #[test]
    fn a_services_stream_uid_follows_its_name() {
        // A uid configured separately from the name would drift, leaving a
        // stale device row on the server after a rename.
        assert_eq!(identity().uid().as_str(), "SERVICE-adsb-feed");
    }

    #[test]
    fn a_published_descriptor_carries_the_name_and_nothing_else() {
        // The boundary this module exists to keep: a descriptor is published to
        // the server and on to the admin UI, so nothing that would let another
        // process impersonate the sidecar may cross into it.
        let identity = identity()
            .with_credential(Secret::new("rsk_supersecret"))
            .with_client_cert("/etc/rustak/adsb.pem", "/etc/rustak/adsb.key");

        let descriptor = ServiceDescriptor::from(&identity);
        let published = serde_json::to_string(&descriptor).unwrap();

        assert_eq!(descriptor.name.as_str(), "adsb-feed");
        assert!(!published.contains("rsk_supersecret"), "{published}");
        assert!(!published.contains("adsb.key"), "{published}");
    }

    #[test]
    fn a_service_token_never_appears_in_a_debug_dump() {
        // Sidecars log their own configuration at start-up; that has to stay a
        // safe thing to do.
        let identity = identity().with_credential(Secret::new("rsk_supersecret"));

        let printed = format!("{identity:?}");
        assert!(!printed.contains("rsk_supersecret"), "{printed}");
        assert!(printed.contains("Secret(***)"), "{printed}");
    }

    #[test]
    fn paths_are_shown_because_they_are_what_a_start_up_failure_is_about() {
        let identity = identity().with_client_cert("/etc/rustak/adsb.pem", "/etc/rustak/adsb.key");

        let printed = format!("{identity:?}");
        assert!(printed.contains("adsb.pem"), "{printed}");
        assert!(printed.contains("adsb.key"), "{printed}");
    }

    #[test]
    fn mutual_tls_needs_both_halves_of_the_certificate() {
        // Half a client certificate is a connection that fails at handshake
        // with an opaque TLS error, so the identity says up front whether it
        // has what it needs.
        assert!(!identity().has_client_cert());
        assert!(
            identity()
                .with_client_cert("/etc/rustak/adsb.pem", "/etc/rustak/adsb.key")
                .has_client_cert()
        );
    }

    #[test]
    fn every_attachment_is_readable_back_out() {
        let identity = identity()
            .with_credential(Secret::new("rsk_token"))
            .with_client_cert("/c.pem", "/k.pem")
            .with_truststore("/t.pem");

        assert_eq!(identity.credential().map(Secret::expose), Some("rsk_token"));
        assert_eq!(identity.certificate(), Some(Path::new("/c.pem")));
        assert_eq!(identity.key(), Some(Path::new("/k.pem")));
        assert_eq!(identity.truststore(), Some(Path::new("/t.pem")));
        assert_eq!(identity.name().as_str(), "adsb-feed");
    }
}
