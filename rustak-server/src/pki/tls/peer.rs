//! Carrying the client's certificate from the handshake to the handler.
//!
//! rustls verifies a client certificate during the handshake and then keeps it
//! on the connection. Actix hands a request no way to reach that on its own, so
//! [`on_connect_capture`] is registered through `HttpServer::on_connect` and
//! lifts what matters into the connection's extensions, where a handler reads
//! it with `req.conn_data::<PeerCertificate>()`. The streaming listener does
//! the same through [`from_tokio_rustls`] when it accepts a connection.
//!
//! # Only what has already been verified
//!
//! A [`PeerCertificate`] exists at all only because the verifier accepted it,
//! so the common name in one has been proven to belong to a certificate our
//! authority issued and has not taken back. That is what makes it usable as an
//! identity; the raw certificate on its own would not be.

use std::any::Any;

use rustls::pki_types::CertificateDer;

use crate::pki::pem::sha256_fingerprint;

/// The client certificate a connection authenticated with.
#[derive(Clone, PartialEq, Eq)]
pub struct PeerCertificate {
    /// The certificate itself.
    pub der: CertificateDer<'static>,

    /// Its SHA-256, which is the key the `certificates` table is read by.
    pub fingerprint: String,

    /// Its common name, which is the username — when it has one.
    pub common_name: Option<String>,

    /// Its serial number, lowercase hexadecimal, for display and audit.
    pub serial_hex: String,
}

impl std::fmt::Debug for PeerCertificate {
    /// Written out because the DER is several kilobytes of noise in a log line
    /// and the fingerprint identifies it exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerCertificate")
            .field("common_name", &self.common_name)
            .field("fingerprint", &self.fingerprint)
            .field("serial_hex", &self.serial_hex)
            .finish_non_exhaustive()
    }
}

impl PeerCertificate {
    /// Reads what we need out of a verified certificate.
    pub fn from_der(der: &CertificateDer<'_>) -> Self {
        let owned = CertificateDer::from(der.to_vec());
        let (common_name, serial_hex) = match x509_parser::parse_x509_certificate(&owned) {
            Ok((_, certificate)) => (
                certificate
                    .subject()
                    .iter_common_name()
                    .next()
                    .and_then(|attribute| attribute.as_str().ok())
                    .map(str::to_owned),
                hex::encode(certificate.raw_serial()),
            ),
            // The verifier parsed it to accept it, so this is unreachable in
            // practice; an unparseable certificate still yields a fingerprint,
            // which is enough to look the row up and to log about.
            Err(_) => (None, String::new()),
        };

        Self {
            fingerprint: sha256_fingerprint(&owned),
            der: owned,
            common_name,
            serial_hex,
        }
    }
}

/// Lifts the peer certificate into a connection's extensions.
///
/// Registered as `HttpServer::new(..).on_connect(on_connect_capture)`. A
/// connection without a certificate — which the Marti listener allows, so that
/// enrolment can happen over it — leaves the extensions untouched, and
/// `req.conn_data::<PeerCertificate>()` answers `None`.
pub fn on_connect_capture(connection: &dyn Any, extensions: &mut actix_web::dev::Extensions) {
    let Some(stream) = connection
        .downcast_ref::<actix_tls::accept::rustls_0_23::TlsStream<actix_web::rt::net::TcpStream>>()
    else {
        // A plaintext listener, or a stream type actix handed us that we do not
        // know: either way there is no certificate to lift, not an error.
        return;
    };

    if let Some(certificate) = from_tokio_rustls(stream) {
        extensions.insert(certificate);
    }
}

/// Reads the peer certificate off an accepted TLS stream.
///
/// The streaming listener uses this directly, and [`on_connect_capture`] uses
/// it through actix-tls' wrapper, which dereferences to the same type.
pub fn from_tokio_rustls<IO>(
    stream: &tokio_rustls::server::TlsStream<IO>,
) -> Option<PeerCertificate> {
    stream
        .get_ref()
        .1
        .peer_certificates()?
        .first()
        .map(PeerCertificate::from_der)
}

/// The common name of a certificate, which is what TAK resolves to a user.
pub fn common_name(der: &CertificateDer<'_>) -> Option<String> {
    x509_parser::parse_x509_certificate(der)
        .ok()?
        .1
        .subject()
        .iter_common_name()
        .next()
        .and_then(|attribute| attribute.as_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::testing::TestAuthority;

    #[tokio::test]
    async fn a_certificate_yields_the_identity_the_handshake_proved() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");
        let peer = PeerCertificate::from_der(&client.der);

        assert_eq!(peer.common_name.as_deref(), Some("alice"));
        assert_eq!(peer.fingerprint, client.fingerprint);
        assert_eq!(peer.serial_hex.len(), 32, "a 128-bit serial");
        assert_eq!(peer.der, client.der);
    }

    #[tokio::test]
    async fn the_common_name_helper_agrees_with_the_extraction() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");

        assert_eq!(
            common_name(&client.der),
            PeerCertificate::from_der(&client.der).common_name
        );
        assert_eq!(
            common_name(&authority.certificate()).as_deref(),
            Some("rustak CA")
        );
    }

    #[test]
    fn something_that_is_not_a_certificate_still_has_a_fingerprint() {
        let rubbish = CertificateDer::from(vec![0x30, 0x00]);
        let peer = PeerCertificate::from_der(&rubbish);

        assert!(peer.common_name.is_none());
        assert_eq!(
            peer.fingerprint,
            crate::pki::pem::sha256_fingerprint(&rubbish)
        );
        assert!(common_name(&rubbish).is_none());
    }

    #[tokio::test]
    async fn a_peer_certificate_never_renders_its_der() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");
        let rendered = format!("{:?}", PeerCertificate::from_der(&client.der));

        assert!(rendered.contains("alice"));
        assert!(!rendered.contains(&hex::encode(&client.der)));
    }

    #[test]
    fn a_connection_that_is_not_a_tls_stream_contributes_nothing() {
        let mut extensions = actix_web::dev::Extensions::new();

        on_connect_capture(&"a plaintext connection", &mut extensions);

        assert!(extensions.get::<PeerCertificate>().is_none());
    }
}
