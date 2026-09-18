//! Signing a client certificate from a request.
//!
//! # The subject is ours
//!
//! rcgen's [`rcgen::CertificateSigningRequestParams`] carries the whole request
//! — its subject, its alternative names and any extension it asked for — and
//! would happily sign all of it. Every one of those is replaced here before the
//! certificate is issued, and only the public key survives.
//!
//! That is the difference between a certificate authority and a notary. The
//! common name of a client certificate is what the stream and Marti listeners
//! resolve to a user; honouring the one in the request would let anybody able
//! to enrol mint a certificate naming somebody else, and the extended key usage
//! decides what the certificate may be used for at all.
//!
//! # Why the validity is clamped
//!
//! A certificate issued past the expiry of the authority that signed it stops
//! verifying the moment the authority does, but looks valid until then. The
//! window is therefore cut to a day inside the CA's own, which is a short
//! certificate rather than a broken one.

use chrono::{DateTime, Duration, Utc};
use rustak_core::prelude::*;
use rustls_pki_types::CertificateDer;

use super::ca::CaMaterial;
use super::csr::{CsrKey, ParsedCsr};
use super::pem::sha256_fingerprint;
use super::serial::{random_serial, serial_hex};

/// The OID TAK Server adds to a client certificate when the enrolment carried a
/// `version` query parameter: PKCS#9 `challengePassword`, used out of its
/// original meaning as a "this client understands channels" marker.
///
/// ATAK performs no extended-key-usage inspection of its own, so this is for
/// parity with the rest of the TAK ecosystem rather than a requirement.
pub const CHANNELS_MARKER_OID: &[u64] = &[1, 2, 840, 113549, 1, 9, 7];

/// How far the validity window is kept inside the authority's own.
const CA_EXPIRY_MARGIN_DAYS: i64 = 1;

/// Advice for a failure that is ours rather than the client's.
const ADVICE_REPORT: &[&str] =
    &["This is unexpected; please report it with the surrounding log entries."];

/// What a caller asks for when it wants a client certificate.
#[derive(Debug, Clone)]
pub struct IssueRequest<'a> {
    /// The authenticated user the certificate identifies. Becomes its `CN`.
    pub username: &'a Username,

    /// The device UID the client sent as `clientUid`, recorded alongside the
    /// certificate so a lost device can be revoked without revoking the user.
    pub client_uid: Option<&'a str>,

    /// How long the certificate is valid, before clamping to the authority.
    pub validity: Duration,

    /// How far `notBefore` is backdated, so a device with a skewed clock does
    /// not reject the certificate it was just handed.
    pub not_before_skew: Duration,

    /// Whether to add [`CHANNELS_MARKER_OID`].
    pub channels_marker: bool,
}

impl<'a> IssueRequest<'a> {
    /// A request following `[pki]`, for the user enrolling.
    ///
    /// `channels_capable` is whether the enrolment carried a `version` query
    /// parameter; the marker is only added when the configuration also allows
    /// it.
    pub fn new(
        username: &'a Username,
        pki: &crate::config::PkiConfig,
        channels_capable: bool,
    ) -> Self {
        Self {
            username,
            client_uid: None,
            validity: pki.client_cert_validity,
            not_before_skew: Duration::minutes(DEFAULT_SKEW_MINUTES),
            channels_marker: pki.channels_marker_eku && channels_capable,
        }
    }

    /// Records the device UID the client enrolled with.
    pub fn for_device(mut self, client_uid: &'a str) -> Self {
        self.client_uid = Some(client_uid);
        self
    }
}

/// Five minutes: enough for the clock skew a phone that has been offline
/// accumulates, short enough that a certificate is never usable long before it
/// was asked for.
pub const DEFAULT_SKEW_MINUTES: i64 = 5;

/// A certificate we have just signed.
#[derive(Clone)]
pub struct IssuedCert {
    /// The certificate itself.
    pub der: CertificateDer<'static>,

    /// Its serial number, lowercase hexadecimal.
    pub serial_hex: String,

    /// The SHA-256 of its DER, which is how rustak identifies it everywhere.
    pub fingerprint: String,

    /// The subject we gave it, rendered for display and audit.
    pub subject: String,

    /// The common name, which is the username.
    pub common_name: String,

    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

impl std::fmt::Debug for IssuedCert {
    /// Written out so that a `{:?}` on something holding one prints what the
    /// certificate *is* rather than a page of DER.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedCert")
            .field("subject", &self.subject)
            .field("serial_hex", &self.serial_hex)
            .field("fingerprint", &self.fingerprint)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// Signs a client certificate for the public key in `csr`.
///
/// `name_entries` are the relative distinguished names that follow the common
/// name, in configuration order. Everything the request asked for other than
/// its public key is discarded.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error: the request has already been parsed
/// and checked by this point, so a failure here is ours.
#[instrument("pki.issue.client", skip_all, fields(user = %request.username), err(Display))]
pub fn issue_client_cert(
    ca: &CaMaterial,
    csr: &ParsedCsr,
    name_entries: &[(&str, &str)],
    request: &IssueRequest<'_>,
) -> Result<IssuedCert, Error> {
    let mut plan = rcgen::CertificateSigningRequestParams::from_der(&csr.der.as_slice().into())
        .or_system_err(ADVICE_REPORT)?;

    let now = Utc::now();
    let not_before = now - request.not_before_skew;
    let not_after = (now + request.validity).min(latest_usable(ca));

    if not_after <= now {
        return Err(human_errors::user(
            "This installation's certificate authority has expired, so no certificate can be issued.",
            &[
                "An administrator must create a new authority, after which every device has to enrol again.",
                "Increase 'ca_validity' under [pki] before creating the replacement.",
            ],
        ));
    }

    let serial = random_serial();
    let subject = subject_of(request.username, name_entries);

    // Everything the client asked for goes here, in one place, so that a future
    // reader can see that nothing from the request survives except its key.
    plan.params.distinguished_name = distinguished_name(request.username, name_entries);
    plan.params.subject_alt_names.clear();
    plan.params.custom_extensions.clear();
    plan.params.crl_distribution_points.clear();
    plan.params.name_constraints = None;
    plan.params.is_ca = rcgen::IsCa::ExplicitNoCa;
    plan.params.key_usages = key_usages(&csr.key);
    plan.params.extended_key_usages = extended_key_usages(request.channels_marker);
    plan.params.serial_number = Some(rcgen::SerialNumber::from_slice(&serial));
    plan.params.not_before = asn1_time(not_before)?.to_datetime();
    plan.params.not_after = asn1_time(not_after)?.to_datetime();
    plan.params.use_authority_key_identifier_extension = true;
    plan.params.key_identifier_method = rcgen::KeyIdMethod::Sha256;

    let certificate = plan.signed_by(ca.issuer()).or_system_err(ADVICE_REPORT)?;
    let der = CertificateDer::from(certificate.der().to_vec());

    info!(
        subject = %subject,
        serial = %serial_hex(&serial),
        not_after = %not_after,
        key = %csr.key.describe(),
        encoding = %csr.encoding.as_str(),
        requested_sans = csr.requested_sans,
        "Issued a client certificate."
    );

    Ok(IssuedCert {
        fingerprint: sha256_fingerprint(&der),
        common_name: request.username.as_str().to_owned(),
        serial_hex: serial_hex(&serial),
        subject,
        der,
        not_before,
        not_after,
    })
}

/// The latest instant a certificate we issue may expire.
fn latest_usable(ca: &CaMaterial) -> DateTime<Utc> {
    ca.not_after() - Duration::days(CA_EXPIRY_MARGIN_DAYS)
}

/// The subject, as a string, in the order it is encoded.
fn subject_of(username: &Username, name_entries: &[(&str, &str)]) -> String {
    let mut subject = format!("CN={username}");

    for (name, value) in name_entries {
        if dn_type(name).is_some() {
            subject.push_str(&format!(",{name}={value}"));
        }
    }

    subject
}

/// The subject rcgen encodes: the common name, then the configured entries.
fn distinguished_name(
    username: &Username,
    name_entries: &[(&str, &str)],
) -> rcgen::DistinguishedName {
    let mut name = rcgen::DistinguishedName::new();

    name.push(rcgen::DnType::CommonName, username.as_str());

    for (entry, value) in name_entries {
        let Some(kind) = dn_type(entry) else {
            warn!(entry = %entry, "Ignoring an unrecognised name entry under [pki] name_entries.");
            continue;
        };

        name.push(kind, *value);
    }

    name
}

/// The key usages a certificate for this key type may assert.
///
/// `keyEncipherment` only means anything for an RSA key — it is the usage a
/// TLS 1.2 RSA key exchange needs — and asserting it on an elliptic-curve key
/// is a contradiction some verifiers refuse. `keyAgreement` is what the curve
/// gets instead.
fn key_usages(key: &CsrKey) -> Vec<rcgen::KeyUsagePurpose> {
    let mut usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::ContentCommitment,
    ];

    match key {
        CsrKey::Rsa { .. } => usages.push(rcgen::KeyUsagePurpose::KeyEncipherment),
        CsrKey::Ecdsa { .. } => usages.push(rcgen::KeyUsagePurpose::KeyAgreement),
        CsrKey::Other(_) => {}
    }

    usages
}

/// The extended key usages: client authentication, and the TAK marker when it
/// was asked for.
fn extended_key_usages(channels_marker: bool) -> Vec<rcgen::ExtendedKeyUsagePurpose> {
    let mut usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];

    if channels_marker {
        usages.push(rcgen::ExtendedKeyUsagePurpose::Other(
            CHANNELS_MARKER_OID.to_vec(),
        ));
    }

    usages
}

/// Converts one of our timestamps into the type rcgen dates a certificate with.
///
/// rcgen's `not_before`/`not_after` are `time::OffsetDateTime`, and `time` is
/// not a dependency of ours — `x509-parser` is, it already resolves to the same
/// `time`, and `ASN1Time::to_datetime` hands one over. A direct dependency
/// would buy a second date library for one conversion, and the certificate
/// authority already dates its own certificates by the day rather than the
/// second; a client certificate needs the minute, so that this one can be
/// backdated by exactly the skew asked for.
fn asn1_time(at: DateTime<Utc>) -> Result<x509_parser::time::ASN1Time, Error> {
    x509_parser::time::ASN1Time::from_timestamp(at.timestamp()).or_system_err(ADVICE_REPORT)
}

/// Maps the short attribute names an operator writes to rcgen's types.
fn dn_type(name: &str) -> Option<rcgen::DnType> {
    match name.to_ascii_uppercase().as_str() {
        "CN" => Some(rcgen::DnType::CommonName),
        "O" => Some(rcgen::DnType::OrganizationName),
        "OU" => Some(rcgen::DnType::OrganizationalUnitName),
        "C" => Some(rcgen::DnType::CountryName),
        "L" => Some(rcgen::DnType::LocalityName),
        "ST" | "S" => Some(rcgen::DnType::StateOrProvinceName),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PkiConfig;
    use crate::crypto::SecretStore;
    use crate::db::Database;
    use crate::pki::ca::load_or_create_root_ca;
    use crate::pki::csr::parse_csr;
    use crate::pki::keys::{KeyType, generate_key};
    use crate::pki::serial::SERIAL_BYTES;

    const ENTRIES: &[(&str, &str)] = &[("O", "rustak"), ("OU", "EUD")];

    async fn authority() -> (CaMaterial, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = PkiConfig {
            key_type: KeyType::EcdsaP256,
            ..PkiConfig::default()
        };
        let ca = load_or_create_root_ca(
            &Database::open_in_memory().await.unwrap(),
            &SecretStore::ephemeral(),
            &config,
            dir.path(),
        )
        .await
        .unwrap();

        (ca, dir)
    }

    fn csr(common_name: &str, kind: KeyType) -> ParsedCsr {
        let key = generate_key(kind).unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["not.ours.example".to_owned()]).unwrap();
        let mut name = rcgen::DistinguishedName::new();

        name.push(rcgen::DnType::CommonName, common_name);
        name.push(rcgen::DnType::OrganizationName, "somebody else");
        params.distinguished_name = name;

        parse_csr(params.serialize_request(&key).unwrap().der()).unwrap()
    }

    fn alice() -> Username {
        Username::parse("alice").unwrap()
    }

    fn request(username: &Username) -> IssueRequest<'_> {
        IssueRequest {
            username,
            client_uid: None,
            validity: Duration::days(365),
            not_before_skew: Duration::minutes(DEFAULT_SKEW_MINUTES),
            channels_marker: false,
        }
    }

    fn parsed(issued: &IssuedCert) -> x509_parser::certificate::X509Certificate<'_> {
        x509_parser::parse_x509_certificate(&issued.der)
            .expect("we issued a parseable certificate")
            .1
    }

    #[tokio::test]
    async fn the_subject_is_ours_in_configuration_order() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        assert_eq!(issued.subject, "CN=alice,O=rustak,OU=EUD");
        assert_eq!(
            parsed(&issued).subject().to_string(),
            "CN=alice, O=rustak, OU=EUD"
        );
    }

    #[tokio::test]
    async fn nothing_the_request_asked_for_survives() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let source = csr("alice", KeyType::EcdsaP256);

        assert_eq!(source.requested_sans, 1, "the request did ask for a name");

        let issued = issue_client_cert(&ca, &source, ENTRIES, &request(&user)).unwrap();
        let certificate = parsed(&issued);

        assert!(
            certificate.subject_alternative_name().unwrap().is_none(),
            "a name the client asked for must not be issued"
        );
        assert!(
            !certificate.subject().to_string().contains("somebody else"),
            "the organisation in the request must not be issued"
        );
        assert!(!certificate.is_ca(), "a client certificate signs nothing");
    }

    #[tokio::test]
    async fn the_public_key_does_come_from_the_request() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let source = csr("alice", KeyType::Rsa2048);
        let issued = issue_client_cert(&ca, &source, ENTRIES, &request(&user)).unwrap();

        use x509_parser::certification_request::X509CertificationRequest;
        use x509_parser::prelude::FromDer as _;

        let (_, submitted) = X509CertificationRequest::from_der(&source.der).unwrap();

        assert_eq!(
            parsed(&issued).public_key().raw,
            submitted.certification_request_info.subject_pki.raw,
            "the certificate must certify the key the client holds"
        );
    }

    #[tokio::test]
    async fn a_client_certificate_authenticates_clients_and_nothing_else() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();
        let certificate = parsed(&issued);
        let usages = certificate
            .extended_key_usage()
            .unwrap()
            .expect("a client certificate declares its purpose")
            .value;

        assert!(usages.client_auth);
        assert!(!usages.server_auth);
        assert!(usages.other.is_empty(), "no marker unless it was asked for");
    }

    #[tokio::test]
    async fn the_channels_marker_is_added_only_when_asked_for() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &IssueRequest {
                channels_marker: true,
                ..request(&user)
            },
        )
        .unwrap();

        let certificate = parsed(&issued);
        let usages = certificate.extended_key_usage().unwrap().unwrap().value;
        let expected: Vec<u64> = CHANNELS_MARKER_OID.to_vec();

        assert!(usages.client_auth);
        assert_eq!(
            usages
                .other
                .iter()
                .map(|oid| oid.iter().unwrap().collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![expected]
        );
    }

    #[tokio::test]
    async fn key_usage_follows_the_key_type() {
        let (ca, _dir) = authority().await;
        let user = alice();

        let rsa = issue_client_cert(
            &ca,
            &csr("alice", KeyType::Rsa2048),
            ENTRIES,
            &request(&user),
        )
        .unwrap();
        let ec = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        let (rsa_cert, ec_cert) = (parsed(&rsa), parsed(&ec));
        let rsa_usage = rsa_cert.key_usage().unwrap().unwrap().value;
        let ec_usage = ec_cert.key_usage().unwrap().unwrap().value;

        assert!(rsa_usage.digital_signature() && rsa_usage.non_repudiation());
        assert!(rsa_usage.key_encipherment(), "RSA needs it for TLS 1.2");
        assert!(!rsa_usage.key_agreement());

        assert!(ec_usage.digital_signature() && ec_usage.non_repudiation());
        assert!(ec_usage.key_agreement());
        assert!(
            !ec_usage.key_encipherment(),
            "a curve key cannot encipher a key"
        );
    }

    #[tokio::test]
    async fn the_serial_is_a_hundred_and_twenty_eight_random_bits() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let first = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();
        let second = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        assert_eq!(first.serial_hex.len(), SERIAL_BYTES * 2);
        assert_ne!(first.serial_hex, second.serial_hex);
        assert!(
            parsed(&first).raw_serial()[0] < 0x80,
            "the serial must encode as a positive integer"
        );
        assert_eq!(
            serial_hex(parsed(&first).raw_serial()),
            first.serial_hex,
            "the certificate spells its serial the way the row does"
        );
    }

    /// The subject key identifier a certificate publishes, if it has one.
    fn subject_key_id(certificate: &x509_parser::certificate::X509Certificate<'_>) -> Vec<u8> {
        certificate
            .iter_extensions()
            .find_map(|extension| match extension.parsed_extension() {
                x509_parser::extensions::ParsedExtension::SubjectKeyIdentifier(id) => {
                    Some(id.0.to_vec())
                }
                _ => None,
            })
            .expect("a certificate rcgen issued carries a subject key identifier")
    }

    #[tokio::test]
    async fn the_key_identifiers_point_at_the_authority() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        let certificate = parsed(&issued);
        let authority_key_id = certificate
            .iter_extensions()
            .find_map(|extension| match extension.parsed_extension() {
                x509_parser::extensions::ParsedExtension::AuthorityKeyIdentifier(id) => {
                    id.key_identifier.as_ref().map(|key| key.0.to_vec())
                }
                _ => None,
            })
            .expect("a leaf names the authority that signed it");

        let ca_certificate = x509_parser::parse_x509_certificate(ca.certificate())
            .unwrap()
            .1;

        assert!(!subject_key_id(&certificate).is_empty());
        assert_eq!(
            authority_key_id,
            subject_key_id(&ca_certificate),
            "the authority key identifier must be the CA's subject key identifier"
        );
    }

    #[tokio::test]
    async fn the_validity_starts_before_now_and_is_capped_by_the_authority() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let now = Utc::now();

        let ordinary = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        assert!(ordinary.not_before < now, "backdated for clock skew");
        assert!(now - ordinary.not_before < Duration::minutes(10));
        assert!(
            (ordinary.not_after - now - Duration::days(365))
                .num_seconds()
                .abs()
                < 60
        );

        let greedy = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &IssueRequest {
                validity: Duration::days(100_000),
                ..request(&user)
            },
        )
        .unwrap();

        assert!(
            greedy.not_after < ca.not_after(),
            "nothing may outlive the authority that signed it"
        );
    }

    #[tokio::test]
    async fn the_certificate_verifies_against_the_authority() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        let authority = x509_parser::parse_x509_certificate(ca.certificate())
            .unwrap()
            .1;

        parsed(&issued)
            .verify_signature(Some(authority.public_key()))
            .expect("the chain must verify");
    }

    #[tokio::test]
    async fn an_unrecognised_name_entry_is_dropped_rather_than_refused() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            &[("O", "rustak"), ("NONSENSE", "x")],
            &request(&user),
        )
        .unwrap();

        assert_eq!(issued.subject, "CN=alice,O=rustak");
    }

    #[tokio::test]
    async fn a_request_built_from_the_configuration_follows_it() {
        let user = alice();
        let pki = PkiConfig {
            client_cert_validity: Duration::days(30),
            channels_marker_eku: true,
            ..PkiConfig::default()
        };

        assert!(IssueRequest::new(&user, &pki, true).channels_marker);
        assert!(!IssueRequest::new(&user, &pki, false).channels_marker);
        assert_eq!(
            IssueRequest::new(&user, &pki, true).validity,
            Duration::days(30)
        );

        let off = PkiConfig {
            channels_marker_eku: false,
            ..pki
        };

        assert!(!IssueRequest::new(&user, &off, true).channels_marker);
        assert_eq!(
            IssueRequest::new(&user, &off, true)
                .for_device("ANDROID-1")
                .client_uid,
            Some("ANDROID-1")
        );
    }

    #[tokio::test]
    async fn a_certificate_never_renders_its_own_contents() {
        let (ca, _dir) = authority().await;
        let user = alice();
        let issued = issue_client_cert(
            &ca,
            &csr("alice", KeyType::EcdsaP256),
            ENTRIES,
            &request(&user),
        )
        .unwrap();

        let rendered = format!("{issued:?}");

        assert!(rendered.contains("CN=alice"));
        assert!(!rendered.contains(&hex::encode(&issued.der)));
    }
}
