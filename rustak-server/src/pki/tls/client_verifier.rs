//! Deciding whether to accept the certificate a client presents.
//!
//! # What webpki answers, and what it cannot
//!
//! [`WebPkiClientVerifier`] checks the things that are properties of the
//! certificate itself: that it chains to our authority, that the signatures
//! hold, that it is inside its validity window, and that it claims the
//! `clientAuth` extended key usage. None of that can answer "has this been
//! taken back", because revocation is a fact about our database rather than
//! about the bytes.
//!
//! This verifier wraps it and asks the second question afterwards, against the
//! in-memory [`RevocationCache`]. The order matters: the cache is keyed by
//! fingerprint, and hashing a certificate that has not been proven to chain to
//! us would let anybody probe which fingerprints we have heard of.
//!
//! # Mandatory versus optional
//!
//! The streaming listener requires a certificate, because the certificate *is*
//! the authentication there. The Marti listener asks for one but accepts a
//! connection without it, because the same port serves `/oauth/token` and the
//! enrolment routes, which a device reaches before it has a certificate at all.
//! Which of the two a connection got is then an input to authentication, not a
//! substitute for it.

use std::sync::Arc;

use rustak_core::prelude::*;
use rustls::DistinguishedName;
use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::pki::pem::sha256_fingerprint;
use crate::pki::revoke::{CertRejection, RevocationCache};

/// Our client certificate verifier: webpki's answer, then revocation.
#[derive(Debug)]
pub struct RustakClientVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    revocations: Arc<RevocationCache>,
    mandatory: bool,
}

impl RustakClientVerifier {
    /// Builds a verifier trusting `roots` and consulting `revocations`.
    ///
    /// `mandatory` decides whether a connection without a certificate is
    /// refused at the handshake or allowed through for the application to
    /// authenticate some other way.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the authority certificates
    /// cannot be used as trust anchors.
    pub fn new(
        roots: &[CertificateDer<'static>],
        revocations: Arc<RevocationCache>,
        mandatory: bool,
    ) -> Result<Arc<Self>, Error> {
        // `WebPkiClientVerifier` reaches for the process-wide provider and
        // *panics* without one; start-up installs it, and this makes a test
        // that builds a verifier on its own behave the same way.
        super::install_crypto_provider();

        let mut store = RootCertStore::empty();

        for root in roots {
            store.add(root.clone()).or_system_err(&[
                "This installation's certificate authority could not be used as a trust anchor.",
                "This is unexpected; please report it with the surrounding log entries.",
            ])?;
        }

        // `allow_unauthenticated` is set whatever `mandatory` says, because it
        // is `client_auth_mandatory` below that rustls consults — keeping the
        // decision in one place rather than half here and half in the builder.
        let inner = WebPkiClientVerifier::builder(Arc::new(store))
            .allow_unauthenticated()
            .build()
            .wrap_system_err(
                "The client certificate verifier could not be built.",
                &["This is unexpected; please report it with the surrounding log entries."],
            )?;

        Ok(Arc::new(Self {
            inner,
            revocations,
            mandatory,
        }))
    }

    /// Whether a connection without a certificate is refused.
    pub fn is_mandatory(&self) -> bool {
        self.mandatory
    }
}

impl ClientCertVerifier for RustakClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        self.mandatory
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.inner
            .verify_client_cert(end_entity, intermediates, now)?;

        let fingerprint = sha256_fingerprint(end_entity);

        match self.revocations.is_acceptable(&fingerprint) {
            Ok(()) => Ok(ClientCertVerified::assertion()),
            Err(rejection) => {
                warn!(
                    fingerprint = %fingerprint,
                    reason = %rejection.as_str(),
                    "Refused a client certificate at the handshake."
                );

                Err(rustls::Error::InvalidCertificate(match rejection {
                    CertRejection::Revoked => CertificateError::Revoked,
                    // Not `Revoked`: a certificate we have no record of was
                    // never taken back, and saying so would tell an operator
                    // reading the client's error the wrong thing to look for.
                    CertRejection::Unknown => CertificateError::UnknownIssuer,
                }))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::testing::TestAuthority;

    fn verifier(
        authority: &TestAuthority,
        cache: Arc<RevocationCache>,
        mandatory: bool,
    ) -> Arc<RustakClientVerifier> {
        RustakClientVerifier::new(&[authority.certificate()], cache, mandatory).unwrap()
    }

    fn now() -> UnixTime {
        UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            chrono::Utc::now().timestamp() as u64,
        ))
    }

    #[tokio::test]
    async fn a_certificate_we_issued_and_still_know_about_is_accepted() {
        let authority = TestAuthority::new().await;
        let issued = authority.issue("alice");
        let cache = RevocationCache::new(true);

        cache.note_issued(&issued.fingerprint);

        assert!(
            verifier(&authority, cache, true)
                .verify_client_cert(&issued.der, &[], now())
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_revoked_certificate_is_refused_as_revoked() {
        let authority = TestAuthority::new().await;
        let issued = authority.issue("alice");
        let cache = RevocationCache::new(true);

        cache.note_issued(&issued.fingerprint);
        cache.note_revoked(&issued.fingerprint);

        let error = verifier(&authority, cache, true)
            .verify_client_cert(&issued.der, &[], now())
            .expect_err("a revoked certificate must not authenticate");

        assert!(
            matches!(
                error,
                rustls::Error::InvalidCertificate(CertificateError::Revoked)
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_certificate_we_have_no_record_of_is_refused_when_the_register_is_closed() {
        let authority = TestAuthority::new().await;
        let issued = authority.issue("alice");

        let strict = verifier(&authority, RevocationCache::new(true), true);
        let lenient = verifier(&authority, RevocationCache::new(false), true);

        assert!(matches!(
            strict.verify_client_cert(&issued.der, &[], now()),
            Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
        ));
        assert!(
            lenient.verify_client_cert(&issued.der, &[], now()).is_ok(),
            "with require_known_cert off, chaining to us is enough"
        );
    }

    #[tokio::test]
    async fn a_certificate_from_another_authority_never_reaches_the_revocation_check() {
        let ours = TestAuthority::new().await;
        let theirs = TestAuthority::new().await;
        let foreign = theirs.issue("alice");

        // Known and unrevoked in *our* cache, so the only thing that can refuse
        // it is the chain check.
        let cache = RevocationCache::new(true);
        cache.note_issued(&foreign.fingerprint);

        assert!(
            verifier(&ours, cache, true)
                .verify_client_cert(&foreign.der, &[], now())
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_expired_certificate_is_refused() {
        let authority = TestAuthority::new().await;
        let issued = authority.issue("alice");
        let cache = RevocationCache::new(false);
        let long_after = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            (chrono::Utc::now() + chrono::Duration::days(10_000)).timestamp() as u64,
        ));

        assert!(
            verifier(&authority, cache, true)
                .verify_client_cert(&issued.der, &[], long_after)
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_certificate_that_is_not_one_is_refused_rather_than_panicking() {
        let authority = TestAuthority::new().await;
        let rubbish = CertificateDer::from(vec![0x30, 0x00]);

        assert!(
            verifier(&authority, RevocationCache::new(false), true)
                .verify_client_cert(&rubbish, &[], now())
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_mandatory_flag_is_what_rustls_reads() {
        let authority = TestAuthority::new().await;
        let required = verifier(&authority, RevocationCache::new(false), true);
        let optional = verifier(&authority, RevocationCache::new(false), false);

        assert!(required.offer_client_auth() && required.client_auth_mandatory());
        assert!(required.is_mandatory());
        assert!(optional.offer_client_auth() && !optional.client_auth_mandatory());
        assert!(!optional.is_mandatory());
    }

    #[tokio::test]
    async fn the_authority_is_hinted_so_clients_pick_the_right_certificate() {
        let authority = TestAuthority::new().await;
        let verifier = verifier(&authority, RevocationCache::new(false), true);

        assert_eq!(
            verifier.root_hint_subjects().len(),
            1,
            "a client with several certificates needs the hint to choose"
        );
        assert!(!verifier.supported_verify_schemes().is_empty());
    }
}
