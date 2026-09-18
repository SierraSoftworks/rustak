//! One handle for everything the certificate authority does.
//!
//! The modules beside this one are deliberately small and independent —
//! parsing a request, signing one, writing a bundle, checking a handshake — and
//! none of them knows about the others. [`Pki`] is what ties them into the
//! three operations the rest of the server actually performs: enrol a device,
//! take a certificate back, and build a listener.
//!
//! # Why enrolment is one call
//!
//! Issuing a certificate and recording it are not two things. A certificate
//! signed but not written down is one `require_known_cert` refuses at the next
//! handshake and nobody can revoke; a row written for a certificate that was
//! never signed is a phantom in the administrator's list. [`Pki::enroll`]
//! therefore parses, checks, signs, records and primes the revocation cache in
//! that order, and the route that calls it has no way to do half of it.
//!
//! # What lives here and what does not
//!
//! The public listener's certificate is not this module's business: it may come
//! from ACME or from files, and `web::tls` decides. What is here is the
//! *internal* certificate the Marti and streaming listeners present, which our
//! own authority issues, and the resolver that lets it be replaced without a
//! restart.

use std::sync::Arc;

use rustak_api::{AuditCategory, AuditOutcome, CertificateKind, CertificateSource};
use rustak_core::prelude::*;
use rustls::ServerConfig;
use rustls::pki_types::CertificateDer;

use super::ca::{CaMaterial, load_or_create_root_ca};
use super::csr::{CsrPolicy, parse_csr, warn_on_subject_mismatch};
use super::issue::{IssueRequest, IssuedCert, issue_client_cert};
use super::p12::P12Options;
use super::revoke::{RevocationCache, RevokeReason};
use super::server_cert::load_or_issue as load_or_issue_server_cert;
use super::tls::{
    HotSwapCertResolver, RustakClientVerifier, marti_server_config, stream_server_config,
};
use crate::config::PkiConfig;
use crate::crypto::SecretStore;
use crate::db::repos::NewCertificate;
use crate::db::{AuditEntry, AuditStore as _, Database};

/// Which endpoint a certificate came out of, for the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssuedVia {
    /// `POST /Marti/api/tls/signClient/v2` answering JSON. CloudTAK.
    EnrollV2Json,

    /// `POST /Marti/api/tls/signClient/v2` answering XML. ATAK.
    EnrollV2Xml,

    /// The pre-v2 endpoint, which answers with a PKCS#12 bundle.
    EnrollV1P12,

    /// An administrator built a configuration package.
    AdminPackage,
}

impl IssuedVia {
    /// The value the `issued_via` column accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnrollV2Json => "enroll_v2_json",
            Self::EnrollV2Xml => "enroll_v2_xml",
            Self::EnrollV1P12 => "enroll_v1_p12",
            Self::AdminPackage => "admin_package",
        }
    }

    /// How the certificate came to exist, as the `source` column records it.
    fn source(self) -> CertificateSource {
        match self {
            Self::AdminPackage => CertificateSource::AdminPackage,
            _ => CertificateSource::Enrollment,
        }
    }
}

/// Everything an enrolment supplies.
#[derive(Debug, Clone)]
pub struct Enrollment<'a> {
    /// The authenticated user. The certificate's common name, and the only
    /// name it will carry whatever the request asked for.
    pub username: &'a Username,

    /// The signing request body, exactly as the client sent it.
    pub csr_body: &'a [u8],

    /// The `clientUid` query parameter. Recorded, never validated as a shape:
    /// CloudTAK sends `"alice (ETL)"`.
    pub client_uid: Option<&'a str>,

    /// The account row, so the certificate can be listed against it.
    pub user_id: Option<UserId>,

    /// The device row, where one has been created.
    pub device_id: Option<DeviceId>,

    /// The credential spent to get here, so revoking it revokes this too.
    pub credential_id: Option<CredentialId>,

    /// The endpoint it came from.
    pub issued_via: IssuedVia,

    /// Whether the request carried a `version` parameter, which is TAK's
    /// marker for a client that understands channels.
    pub channels_capable: bool,
}

/// The certificate authority, as the rest of the server sees it.
#[derive(Debug)]
pub struct Pki {
    ca: CaMaterial,
    config: PkiConfig,
    revocations: Arc<RevocationCache>,
    resolver: Arc<HotSwapCertResolver>,
}

impl Pki {
    /// Loads the authority, issues or reloads the internal listener
    /// certificate, and primes the revocation cache.
    ///
    /// `names` and `addresses` are what the internal certificate should cover;
    /// `web::tls::server_names` works them out from the configuration and the
    /// settings the wizard stored.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the authority or the
    /// certificate cannot be read or written, and a
    /// [`human_errors::Kind::User`] error when no host name is configured.
    #[instrument("pki.load", skip_all, err(Display))]
    pub async fn load(
        db: &Database,
        secrets: &SecretStore,
        config: &PkiConfig,
        data_dir: &std::path::Path,
        names: &[String],
        addresses: &[std::net::IpAddr],
    ) -> Result<Arc<Self>, Error> {
        let ca = load_or_create_root_ca(db, secrets, config, data_dir).await?;
        let certificate =
            load_or_issue_server_cert(db, secrets, config, &ca, names, addresses).await?;

        super::tls::install_crypto_provider();

        let certified = rustls::sign::CertifiedKey::from_der(
            certificate.chain,
            certificate.key,
            &rustls::crypto::aws_lc_rs::default_provider(),
        )
        .wrap_system_err(
            "This server's own certificate and its key do not go together.",
            &["This is unexpected; please report it with the surrounding log entries."],
        )?;

        let revocations = RevocationCache::from_config(config);
        revocations.reload(db).await?;

        Ok(Arc::new(Self {
            ca,
            config: config.clone(),
            revocations,
            resolver: HotSwapCertResolver::new(Some(Arc::new(certified))),
        }))
    }

    /// The root authority.
    pub fn ca(&self) -> &CaMaterial {
        &self.ca
    }

    /// The chain an enrolment response returns, and a truststore holds.
    pub fn chain(&self) -> Vec<CertificateDer<'static>> {
        self.ca.chain_der()
    }

    /// Which certificates the listeners will accept.
    pub fn revocations(&self) -> &Arc<RevocationCache> {
        &self.revocations
    }

    /// The certificate the Marti and streaming listeners present, replaceable
    /// while they are running.
    pub fn resolver(&self) -> &Arc<HotSwapCertResolver> {
        &self.resolver
    }

    /// `[pki]`, for the callers that need a setting we do not wrap.
    pub fn config(&self) -> &PkiConfig {
        &self.config
    }

    /// What a signing request has to satisfy.
    pub fn csr_policy(&self) -> CsrPolicy {
        CsrPolicy::from_config(&self.config)
    }

    /// The name entries every subject we issue carries after its common name.
    pub fn name_entries(&self) -> Vec<(&str, &str)> {
        self.config.subject_entries()
    }

    /// How a PKCS#12 bundle for `friendly_name` is written.
    pub fn p12_options<'a>(&'a self, friendly_name: &'a str) -> P12Options<'a> {
        P12Options::from_config(&self.config, friendly_name)
    }

    /// Enrols a device: parse, check, sign, record.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the request is malformed, is
    /// for somebody else, or uses a key the policy refuses; a
    /// [`human_errors::Kind::System`] error when signing or recording fails.
    #[instrument("pki.enroll", skip_all, fields(user = %enrollment.username), err(Display))]
    pub async fn enroll(
        &self,
        db: &Database,
        enrollment: Enrollment<'_>,
    ) -> Result<IssuedCert, Error> {
        let csr = parse_csr(enrollment.csr_body)?;
        let entries = self.name_entries();

        self.csr_policy().validate(&csr, enrollment.username)?;
        warn_on_subject_mismatch(&csr, &entries);

        let mut request = IssueRequest::new(
            enrollment.username,
            &self.config,
            enrollment.channels_capable,
        );

        if let Some(uid) = enrollment.client_uid {
            request = request.for_device(uid);
        }

        let issued = issue_client_cert(&self.ca, &csr, &entries, &request)?;

        self.record(db, &issued, &enrollment).await?;

        // After the row, never before: a fingerprint the cache accepts but the
        // database has no record of is one nothing can revoke.
        self.revocations.note_issued(&issued.fingerprint);

        db.record(
            AuditEntry::new(
                AuditCategory::Enrollment,
                "certificate.issued",
                AuditOutcome::Success,
            )
            .subject(enrollment.username)
            .actor(enrollment.username)
            .message(format!(
                "A client certificate was issued to {} and expires {}.",
                enrollment.username,
                issued.not_after.format("%Y-%m-%d")
            ))
            .detail(serde_json::json!({
                "fingerprint": issued.fingerprint,
                "serial": issued.serial_hex,
                "client_uid": enrollment.client_uid,
                "issued_via": enrollment.issued_via.as_str(),
                "csr_encoding": csr.encoding.as_str(),
                "key": csr.key.describe(),
                "requested_sans_dropped": csr.requested_sans,
            })),
        )
        .await?;

        Ok(issued)
    }

    /// Takes a certificate back and drops the connections holding it.
    ///
    /// # Errors
    ///
    /// As [`super::revoke::revoke`].
    pub async fn revoke(
        &self,
        db: &Database,
        fingerprint: &str,
        reason: RevokeReason,
        actor: Option<&Username>,
    ) -> Result<bool, Error> {
        super::revoke::revoke(db, &self.revocations, fingerprint, reason, actor).await
    }

    /// A verifier for a listener, trusting this installation's authority.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the authority cannot be
    /// used as a trust anchor.
    pub fn client_verifier(&self, mandatory: bool) -> Result<Arc<RustakClientVerifier>, Error> {
        RustakClientVerifier::new(
            &[self.ca.certificate().clone()],
            Arc::clone(&self.revocations),
            mandatory,
        )
    }

    /// The Marti listener's TLS configuration.
    ///
    /// `required` follows `[web.marti] client_cert`: a device that has not
    /// enrolled yet reaches this port to do so, so an installation that has not
    /// said otherwise asks for a certificate without demanding one.
    ///
    /// # Errors
    ///
    /// As [`Pki::client_verifier`].
    pub fn marti_server_config(&self, required: bool) -> Result<ServerConfig, Error> {
        Ok(marti_server_config(
            Arc::clone(&self.resolver),
            self.client_verifier(required)?,
        ))
    }

    /// The streaming listener's TLS configuration. A certificate is always
    /// required: on `:8089` it *is* the authentication.
    ///
    /// # Errors
    ///
    /// As [`Pki::client_verifier`].
    pub fn stream_server_config(&self) -> Result<ServerConfig, Error> {
        Ok(stream_server_config(
            Arc::clone(&self.resolver),
            self.client_verifier(true)?,
        ))
    }

    /// Writes the row that makes an issued certificate revocable.
    async fn record(
        &self,
        db: &Database,
        issued: &IssuedCert,
        enrollment: &Enrollment<'_>,
    ) -> Result<(), Error> {
        db.certificates()
            .create(NewCertificate {
                kind: CertificateKind::Client,
                source: enrollment.issued_via.source(),
                issued_via: Some(enrollment.issued_via.as_str().to_owned()),
                serial_hex: issued.serial_hex.clone(),
                fingerprint: issued.fingerprint.clone(),
                subject_cn: issued.common_name.clone(),
                san: Vec::new(),
                user_id: enrollment.user_id,
                device_id: enrollment.device_id,
                client_uid: enrollment.client_uid.map(str::to_owned),
                credential_id: enrollment.credential_id,
                issuer_id: None,
                der: issued.der.to_vec(),
                // The device generated its own key and kept it; there is
                // nothing of its private half here to seal.
                key_sealed: None,
                not_before: issued.not_before,
                not_after: issued.not_after,
            })
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::keys::{KeyType, generate_key};

    struct Fixture {
        db: Database,
        pki: Arc<Pki>,
        _dir: tempfile::TempDir,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().await.unwrap();
        let config = PkiConfig {
            key_type: KeyType::EcdsaP256,
            name_entries: vec![
                ("O".to_owned(), "rustak".to_owned()),
                ("OU".to_owned(), "EUD".to_owned()),
            ],
            ..PkiConfig::default()
        };

        let pki = Pki::load(
            &db,
            &SecretStore::ephemeral(),
            &config,
            dir.path(),
            &["tak.example.com".to_owned()],
            &[],
        )
        .await
        .unwrap();

        Fixture { db, pki, _dir: dir }
    }

    /// A PEM-armoured request, as ATAK sends one.
    fn csr_body(common_name: &str) -> Vec<u8> {
        let key = generate_key(KeyType::EcdsaP256).unwrap();
        let mut params = rcgen::CertificateParams::default();

        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, common_name);

        params
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap()
            .into_bytes()
    }

    fn enrollment<'a>(username: &'a Username, body: &'a [u8]) -> Enrollment<'a> {
        Enrollment {
            username,
            csr_body: body,
            client_uid: Some("ANDROID-1"),
            user_id: None,
            device_id: None,
            credential_id: None,
            issued_via: IssuedVia::EnrollV2Xml,
            channels_capable: true,
        }
    }

    #[tokio::test]
    async fn enrolling_issues_records_and_makes_the_certificate_usable() {
        let fixture = fixture().await;
        let user = Username::parse("alice").unwrap();
        let body = csr_body("alice");

        let issued = fixture
            .pki
            .enroll(&fixture.db, enrollment(&user, &body))
            .await
            .unwrap();

        assert_eq!(issued.subject, "CN=alice,O=rustak,OU=EUD");
        assert_eq!(
            fixture.pki.revocations().is_acceptable(&issued.fingerprint),
            Ok(()),
            "the handshake must accept it immediately"
        );

        let row = fixture
            .db
            .certificates()
            .get_by_fingerprint(&issued.fingerprint)
            .await
            .unwrap()
            .expect("the certificate is recorded");

        assert_eq!(row.subject_cn, "alice");
        assert_eq!(row.client_uid.as_deref(), Some("ANDROID-1"));
        assert_eq!(row.issued_via.as_deref(), Some("enroll_v2_xml"));
        assert_eq!(row.serial_hex, issued.serial_hex);
        assert_eq!(row.der, issued.der.to_vec());
        assert!(row.key_sealed.is_none(), "we never hold a client's key");
    }

    #[tokio::test]
    async fn enrolling_for_somebody_else_issues_nothing_and_records_nothing() {
        let fixture = fixture().await;
        let user = Username::parse("alice").unwrap();
        let body = csr_body("bob");

        assert!(
            fixture
                .pki
                .enroll(&fixture.db, enrollment(&user, &body))
                .await
                .is_err()
        );
        assert!(
            fixture
                .db
                .certificates()
                .fingerprints()
                .await
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_enrolment_is_audited_with_what_was_dropped() {
        let fixture = fixture().await;
        let user = Username::parse("alice").unwrap();
        let body = csr_body("alice");

        fixture
            .pki
            .enroll(&fixture.db, enrollment(&user, &body))
            .await
            .unwrap();

        let entries = fixture
            .db
            .audit(crate::db::AuditQuery::recent(10).in_category(AuditCategory::Enrollment))
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "certificate.issued");
        assert_eq!(entries[0].subject.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn revoking_through_the_facade_stops_the_certificate_working() {
        let fixture = fixture().await;
        let user = Username::parse("alice").unwrap();
        let body = csr_body("alice");

        let issued = fixture
            .pki
            .enroll(&fixture.db, enrollment(&user, &body))
            .await
            .unwrap();

        assert!(
            fixture
                .pki
                .revoke(
                    &fixture.db,
                    &issued.fingerprint,
                    RevokeReason::DeviceLost,
                    Some(&user)
                )
                .await
                .unwrap()
        );
        assert_eq!(
            fixture.pki.revocations().is_acceptable(&issued.fingerprint),
            Err(crate::pki::revoke::CertRejection::Revoked)
        );
    }

    #[tokio::test]
    async fn a_reload_finds_what_a_previous_run_issued() {
        let fixture = fixture().await;
        let user = Username::parse("alice").unwrap();
        let body = csr_body("alice");

        let issued = fixture
            .pki
            .enroll(&fixture.db, enrollment(&user, &body))
            .await
            .unwrap();

        let reloaded = Pki::load(
            &fixture.db,
            &SecretStore::ephemeral(),
            fixture.pki.config(),
            fixture._dir.path(),
            &["tak.example.com".to_owned()],
            &[],
        )
        .await;

        // A fresh secret store cannot open the sealed CA key, which is the
        // right failure: the certificate authority is not recoverable without
        // the encryption key, and pretending otherwise would hide that.
        assert!(reloaded.is_err());

        // The register itself survives, which is what a real reload reads.
        let cache = RevocationCache::new(true);
        cache.reload(&fixture.db).await.unwrap();

        assert_eq!(cache.is_acceptable(&issued.fingerprint), Ok(()));
    }

    #[tokio::test]
    async fn the_listeners_are_built_from_the_authority_the_facade_loaded() {
        let fixture = fixture().await;

        assert!(fixture.pki.resolver().is_ready());
        assert!(
            fixture
                .pki
                .stream_server_config()
                .unwrap()
                .alpn_protocols
                .is_empty()
        );
        assert!(
            !fixture
                .pki
                .marti_server_config(true)
                .unwrap()
                .alpn_protocols
                .is_empty()
        );
        assert!(fixture.pki.client_verifier(true).unwrap().is_mandatory());
        assert!(!fixture.pki.client_verifier(false).unwrap().is_mandatory());
        assert_eq!(fixture.pki.chain().len(), 1);
        assert_eq!(fixture.pki.chain()[0], *fixture.pki.ca().certificate());
    }

    #[tokio::test]
    async fn the_facade_exposes_the_configuration_the_routes_need() {
        let fixture = fixture().await;

        assert_eq!(
            fixture.pki.name_entries(),
            vec![("O", "rustak"), ("OU", "EUD")]
        );
        assert_eq!(fixture.pki.csr_policy().min_rsa_bits, 2048);
        assert_eq!(fixture.pki.p12_options("alice").password, "atakatak");
        assert!(fixture.pki.p12_options("alice").legacy);
        assert!(fixture.pki.revocations().require_known());
    }

    #[test]
    fn every_endpoint_has_a_stored_name_the_schema_accepts() {
        for via in [
            IssuedVia::EnrollV2Json,
            IssuedVia::EnrollV2Xml,
            IssuedVia::EnrollV1P12,
            IssuedVia::AdminPackage,
        ] {
            assert!(!via.as_str().is_empty());
        }

        assert_eq!(
            IssuedVia::AdminPackage.source(),
            CertificateSource::AdminPackage
        );
        assert_eq!(
            IssuedVia::EnrollV2Json.source(),
            CertificateSource::Enrollment
        );
    }
}
