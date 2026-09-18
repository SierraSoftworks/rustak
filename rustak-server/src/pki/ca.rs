//! The installation's root certificate authority.
//!
//! rustak is its own CA. Every device that streams CoT or calls the Marti API
//! presents a certificate this key signed, and the enrolment package hands the
//! device this certificate to trust in return. Losing the key means re-enrolling
//! every device, which is why it is generated once, sealed, and never rewritten.
//!
//! # Where it lives
//!
//! The certificate and its sealed key are one record in the `pki` partition of
//! the key/value store: a single value one component owns and reads back whole,
//! which is exactly what that store is for. The key is sealed with
//! [`SecretContext::CaKey`], so a database file on its own — a stolen backup,
//! say — does not yield a signing key.
//!
//! The certificate is *also* written to `<data_dir>/pki/ca.crt` on every start.
//! Operators need it constantly (to hand to a reverse proxy, to `curl --cacert`,
//! to drop into a truststore) and telling them to extract it from SQLite would
//! be a poor answer. It is the public half, so there is nothing to protect.
//!
//! # Creation is once, and it races safely
//!
//! Two processes starting together must not each mint a CA and have one win: the
//! loser's devices would be enrolled against a key nothing trusts. The record is
//! therefore written with an insert that does not overwrite, and a caller that
//! loses the race reads back and adopts the certificate that won.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike as _, Utc};
use rand::Rng as _;
use rustak_core::prelude::*;
use rustls_pki_types::CertificateDer;

use super::keys::{KeyType, generate_key, key_pair_from_pkcs8};
use super::pem::{pem_certificate, sha256_fingerprint};
use crate::config::PkiConfig;
use crate::crypto::{Sealed, SecretContext, SecretStore};
use crate::db::{Database, KeyValueStore as _};

/// The key/value partition the PKI keeps its material in.
pub const PKI_PARTITION: &str = "pki";

/// The key the root CA's record lives under.
pub const ROOT_CA_KEY: &str = "root_ca";

/// The directory under `data_dir` the CA certificate is exported to.
const PKI_DIR: &str = "pki";

/// The file the CA certificate is exported as.
const CA_CERT_FILE: &str = "ca.crt";

/// The record format version, so a later change can be told from this one.
const RECORD_VERSION: u8 = 1;

/// How many bytes of randomness a serial number carries.
const SERIAL_BYTES: usize = 16;

/// The certificate identity the root CA's sealed key is bound to.
///
/// Row identifiers start at one, so zero can never collide with a row of
/// `certificates`; it names "the root CA slot" instead. The CA does not live in
/// that table — it is a singleton the server needs before any row exists — but
/// the sealing context still has to be stable and unique, and this is both.
const ROOT_CA_SLOT: CertificateId = CertificateId::new(0);

/// What is stored under `pki`/`root_ca`.
#[derive(Clone, Serialize, Deserialize)]
struct RootCaRecord {
    version: u8,
    key_type: KeyType,
    /// The certificate's DER encoding, base64 so it travels as JSON.
    certificate: String,
    /// The PKCS#8 private key, sealed.
    key: Sealed,
    subject: String,
    serial_hex: String,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
}

/// A loaded root certificate authority, ready to sign.
pub struct CaMaterial {
    certificate: CertificateDer<'static>,
    subject: String,
    serial_hex: String,
    fingerprint: String,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
    key_type: KeyType,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl std::fmt::Debug for CaMaterial {
    /// Written out so that the signing key cannot reach a log through a stray
    /// `{:?}` on something that happens to hold a [`CaMaterial`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaMaterial")
            .field("subject", &self.subject)
            .field("fingerprint", &self.fingerprint)
            .field("not_after", &self.not_after)
            .field("key_type", &self.key_type)
            .finish_non_exhaustive()
    }
}

impl CaMaterial {
    /// The issuer new certificates are signed by.
    pub fn issuer(&self) -> &rcgen::Issuer<'static, rcgen::KeyPair> {
        &self.issuer
    }

    /// The CA's own certificate.
    pub fn certificate(&self) -> &CertificateDer<'static> {
        &self.certificate
    }

    /// The chain a certificate we issue is presented with: just the root.
    pub fn chain_der(&self) -> Vec<CertificateDer<'static>> {
        vec![self.certificate.clone()]
    }

    /// The CA's certificate, PEM encoded.
    pub fn certificate_pem(&self) -> String {
        pem_certificate(&self.certificate)
    }

    /// The CA's subject, rendered for display and audit.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The SHA-256 of the CA certificate, lowercase hexadecimal.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The CA's serial number, lowercase hexadecimal.
    pub fn serial_hex(&self) -> &str {
        &self.serial_hex
    }

    /// When the CA became valid.
    pub fn not_before(&self) -> DateTime<Utc> {
        self.not_before
    }

    /// When the CA expires. Nothing we issue may outlive this.
    pub fn not_after(&self) -> DateTime<Utc> {
        self.not_after
    }

    /// The key algorithm the CA signs with.
    pub fn key_type(&self) -> KeyType {
        self.key_type
    }
}

/// Loads the root CA, creating it on first run.
///
/// Also refreshes `<data_dir>/pki/ca.crt` so that a deleted or stale export
/// comes back without an operator having to ask for it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the stored record cannot be
/// read, decrypted or parsed, and a [`human_errors::Kind::User`] error when the
/// export cannot be written.
#[instrument("pki.ca.load", skip_all, err(Display))]
pub async fn load_or_create_root_ca(
    db: &Database,
    secrets: &SecretStore,
    pki: &PkiConfig,
    data_dir: &Path,
) -> Result<CaMaterial, Error> {
    let record = match db.get::<RootCaRecord>(PKI_PARTITION, ROOT_CA_KEY).await? {
        Some(record) => record,
        None => create_root_ca(db, secrets, pki).await?,
    };

    let material = open_record(secrets, record)?;

    export_certificate(&material, data_dir).await?;

    info!(
        subject = %material.subject,
        fingerprint = %material.fingerprint,
        not_after = %material.not_after,
        "Loaded the root certificate authority."
    );

    Ok(material)
}

/// The path `ca.crt` is exported to.
pub fn ca_certificate_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PKI_DIR).join(CA_CERT_FILE)
}

/// Generates and stores a root CA, or adopts one another process just stored.
async fn create_root_ca(
    db: &Database,
    secrets: &SecretStore,
    pki: &PkiConfig,
) -> Result<RootCaRecord, Error> {
    let key_type = pki.key_type;

    // RSA generation is hundreds of milliseconds of bignum arithmetic, which
    // would otherwise stall every task sharing this worker thread.
    let key = tokio::task::spawn_blocking(move || generate_key(key_type))
        .await
        .or_system_err(&[
            "This is unexpected; please report it with the surrounding log entries.",
        ])??;

    let plan = root_params(pki);
    let certificate = plan.params.self_signed(&key).or_system_err(&[
        "This is unexpected; please report it with the surrounding log entries.",
    ])?;

    let record = RootCaRecord {
        version: RECORD_VERSION,
        key_type,
        certificate: base64_der(certificate.der()),
        key: secrets.seal(
            &key.serialize_der(),
            SecretContext::CaKey {
                certificate: ROOT_CA_SLOT,
            },
        )?,
        subject: plan.subject,
        serial_hex: plan.serial_hex,
        not_before: plan.not_before,
        not_after: plan.not_after,
    };

    if !db
        .insert(PKI_PARTITION, ROOT_CA_KEY, record.clone())
        .await?
    {
        // Another process created one between our read and our write. Its CA is
        // the one devices will be enrolled against, so ours is discarded.
        warn!("Another rustak process created the certificate authority first; using that one.");

        return db
            .get::<RootCaRecord>(PKI_PARTITION, ROOT_CA_KEY)
            .await?
            .ok_or_else(|| {
                human_errors::system(
                    "The certificate authority disappeared while it was being created.",
                    &["This is unexpected; please report it with the surrounding log entries."],
                )
            });
    }

    info!(
        subject = %record.subject,
        key_type = ?key_type,
        not_after = %record.not_after,
        "Created this installation's root certificate authority."
    );

    Ok(record)
}

/// Rebuilds the signing material from a stored record.
fn open_record(secrets: &SecretStore, record: RootCaRecord) -> Result<CaMaterial, Error> {
    if record.version != RECORD_VERSION {
        return Err(human_errors::system(
            format!(
                "The stored certificate authority uses record version {}, which this version of rustak does not understand.",
                record.version
            ),
            &["This usually means rustak was downgraded; run the newer version instead."],
        ));
    }

    let der = decode_der(&record.certificate)?;
    let pkcs8 = secrets.open(
        &record.key,
        SecretContext::CaKey {
            certificate: ROOT_CA_SLOT,
        },
    )?;
    let key = key_pair_from_pkcs8(&pkcs8, record.key_type)?;

    let certificate = CertificateDer::from(der);
    let issuer = rcgen::Issuer::from_ca_cert_der(&certificate, key).or_system_err(&[
        "The stored certificate authority could not be parsed; the record may be corrupt.",
    ])?;

    Ok(CaMaterial {
        fingerprint: sha256_fingerprint(&certificate),
        certificate,
        subject: record.subject,
        serial_hex: record.serial_hex,
        not_before: record.not_before,
        not_after: record.not_after,
        key_type: record.key_type,
        issuer,
    })
}

/// Everything deciding how to issue a root CA produces: the rcgen parameters,
/// and the facts about them we record beside the certificate so they can be
/// read back without parsing it again.
struct RootPlan {
    params: rcgen::CertificateParams,
    subject: String,
    serial_hex: String,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
}

/// Builds the parameters a root CA is issued with, and the metadata we record.
fn root_params(pki: &PkiConfig) -> RootPlan {
    let now = Utc::now();
    let expires = now + pki.ca_validity;

    let mut distinguished_name = rcgen::DistinguishedName::new();
    distinguished_name.push(rcgen::DnType::CommonName, pki.ca_common_name.as_str());

    let mut subject = format!("CN={}", pki.ca_common_name);

    for (name, value) in pki.subject_entries() {
        let Some(kind) = dn_type(name) else {
            warn!(entry = %name, "Ignoring an unrecognised name entry under [pki] name_entries.");
            continue;
        };

        distinguished_name.push(kind, value);
        subject.push_str(&format!(",{name}={value}"));
    }

    let serial = random_serial();

    let mut params = rcgen::CertificateParams::default();

    params.distinguished_name = distinguished_name;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    // `DigitalSignature` alongside the two signing usages because the same key
    // signs certificate revocation lists and OCSP-style material later; a CA
    // reissued to add it would invalidate every truststore in the field.
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&serial));
    // Day granularity, taken from midnight today, so the certificate is already
    // valid however the clocks of the devices reading it happen to be set.
    params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
    params.not_after =
        rcgen::date_time_ymd(expires.year(), expires.month() as u8, expires.day() as u8);
    params.use_authority_key_identifier_extension = false;
    params.key_identifier_method = rcgen::KeyIdMethod::Sha256;

    RootPlan {
        params,
        subject,
        serial_hex: hex::encode(serial),
        not_before: now,
        not_after: expires,
    }
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

/// A 128-bit serial with the top bit cleared, so its DER encoding stays
/// positive without a leading pad byte.
fn random_serial() -> [u8; SERIAL_BYTES] {
    let mut serial = [0u8; SERIAL_BYTES];
    rand::rng().fill_bytes(&mut serial);
    serial[0] &= 0x7f;

    serial
}

/// Writes `ca.crt` where operators expect to find it.
async fn export_certificate(material: &CaMaterial, data_dir: &Path) -> Result<(), Error> {
    let path = ca_certificate_path(data_dir);
    let rendered = material.certificate_pem();

    if matches!(tokio::fs::read_to_string(&path).await, Ok(existing) if existing == rendered) {
        return Ok(());
    }

    let advice =
        &["Check that the directory in [server] data_dir is writable by the user rustak runs as."];

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.wrap_user_err(
            format!("We could not create the directory '{}'.", parent.display()),
            advice,
        )?;
    }

    tokio::fs::write(&path, rendered).await.wrap_user_err(
        format!(
            "We could not write the CA certificate to '{}'.",
            path.display()
        ),
        advice,
    )?;

    info!(path = %path.display(), "Exported the CA certificate for operators.");

    Ok(())
}

/// Base64 for the DER inside the stored record.
fn base64_der(der: &[u8]) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(der)
}

/// Reads the DER back out of a stored record.
fn decode_der(encoded: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .or_system_err(&["The stored certificate authority is corrupt and cannot be read."])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PkiConfig {
        PkiConfig {
            key_type: KeyType::EcdsaP256,
            ..PkiConfig::default()
        }
    }

    async fn setup() -> (Database, SecretStore, tempfile::TempDir) {
        (
            Database::open_in_memory().await.unwrap(),
            SecretStore::ephemeral(),
            tempfile::tempdir().unwrap(),
        )
    }

    fn parsed(material: &CaMaterial) -> x509_parser::certificate::X509Certificate<'_> {
        x509_parser::parse_x509_certificate(material.certificate())
            .expect("rcgen produced a parseable certificate")
            .1
    }

    #[tokio::test]
    async fn a_new_installation_gets_a_certificate_authority() {
        let (db, secrets, dir) = setup().await;

        let ca = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();
        let cert = parsed(&ca);

        assert!(
            cert.is_ca(),
            "the certificate must assert basicConstraints CA:TRUE"
        );
        assert_eq!(ca.subject(), "CN=rustak CA,O=rustak");
        assert!(cert.subject().to_string().contains("CN=rustak CA"));
        assert_eq!(cert.subject(), cert.issuer(), "a root is signed by itself");
    }

    #[tokio::test]
    async fn the_certificate_may_sign_certificates_and_revocation_lists() {
        let (db, secrets, dir) = setup().await;
        let ca = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();

        let cert = parsed(&ca);
        let usage = cert
            .key_usage()
            .unwrap()
            .expect("a CA declares its key usage")
            .value;

        assert!(
            usage.key_cert_sign(),
            "without this it cannot issue anything"
        );
        assert!(usage.crl_sign());
        assert!(usage.digital_signature());
    }

    #[tokio::test]
    async fn the_serial_is_128_bits_and_positive() {
        let (db, secrets, dir) = setup().await;
        let ca = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();

        let serial = hex::decode(ca.serial_hex()).unwrap();

        assert_eq!(serial.len(), SERIAL_BYTES);
        assert!(
            serial[0] < 0x80,
            "a set top bit would encode as a negative integer"
        );
        assert_eq!(random_serial().len(), SERIAL_BYTES);
    }

    #[tokio::test]
    async fn the_validity_follows_the_configured_window() {
        let (db, secrets, dir) = setup().await;
        let pki = PkiConfig {
            ca_validity: chrono::Duration::days(30),
            ..config()
        };

        let ca = load_or_create_root_ca(&db, &secrets, &pki, dir.path())
            .await
            .unwrap();
        let cert = parsed(&ca);
        let span = cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp();

        assert!(ca.not_before() <= Utc::now());
        assert!(
            (28..=31).contains(&(span / 86_400)),
            "{span} seconds is not about 30 days"
        );
    }

    #[tokio::test]
    async fn a_second_start_reloads_the_same_authority() {
        let (db, secrets, dir) = setup().await;

        let first = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();
        let second = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();

        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.certificate(), second.certificate());
        assert_eq!(first.serial_hex(), second.serial_hex());
    }

    #[tokio::test]
    async fn the_key_is_usable_after_a_reload() {
        let (db, secrets, dir) = setup().await;
        load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();

        // The property that matters: the sealed key survives the round trip in
        // a form that can still sign. Issuing is M2's job; signing is not.
        let ca = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["device.example".to_string()]).unwrap();
        let key = generate_key(KeyType::EcdsaP256).unwrap();

        let signed = leaf.signed_by(&key, ca.issuer()).unwrap();
        let (_, parsed_leaf) = x509_parser::parse_x509_certificate(signed.der()).unwrap();

        assert_eq!(
            parsed_leaf.issuer().to_string(),
            parsed(&ca).subject().to_string()
        );
    }

    #[tokio::test]
    async fn the_stored_key_is_sealed_rather_than_readable() {
        let (db, secrets, dir) = setup().await;
        let ca = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();

        let stored: serde_json::Value = db.get(PKI_PARTITION, ROOT_CA_KEY).await.unwrap().unwrap();
        let rendered = stored.to_string();

        assert!(rendered.contains("\"key\""));
        assert!(!rendered.contains("PRIVATE KEY"));
        let rendered_ca = format!("{ca:?}");
        assert!(
            !rendered_ca.contains("PRIVATE") && !rendered_ca.contains(&stored["key"].to_string()),
            "the debug rendering must not carry key material: {rendered_ca}"
        );
    }

    #[tokio::test]
    async fn a_key_sealed_under_a_different_encryption_key_is_refused() {
        let (db, secrets, dir) = setup().await;
        load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();

        let stranger = SecretStore::ephemeral();

        assert!(
            load_or_create_root_ca(&db, &stranger, &config(), dir.path())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_certificate_is_exported_for_operators() {
        let (db, secrets, dir) = setup().await;

        let ca = load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();
        let path = ca_certificate_path(dir.path());
        let exported = tokio::fs::read_to_string(&path).await.unwrap();

        assert_eq!(exported, ca.certificate_pem());
        assert!(exported.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert_eq!(
            super::super::pem::parse_pem_chain(&exported).unwrap()[0].as_ref(),
            ca.certificate().as_ref()
        );

        // A deleted export comes back rather than staying missing.
        tokio::fs::remove_file(&path).await.unwrap();
        load_or_create_root_ca(&db, &secrets, &config(), dir.path())
            .await
            .unwrap();
        assert!(tokio::fs::try_exists(&path).await.unwrap());
    }

    #[tokio::test]
    async fn configured_name_entries_land_in_the_subject_in_order() {
        let (db, secrets, dir) = setup().await;
        let pki = PkiConfig {
            name_entries: vec![
                ("O".to_string(), "Sierra".to_string()),
                ("OU".to_string(), "EUD".to_string()),
                ("ZZ".to_string(), "ignored".to_string()),
            ],
            ..config()
        };

        let ca = load_or_create_root_ca(&db, &secrets, &pki, dir.path())
            .await
            .unwrap();

        assert_eq!(
            ca.subject(),
            "CN=rustak CA,O=Sierra,OU=EUD",
            "an attribute we cannot encode is dropped from the recorded subject too"
        );
        let rendered = parsed(&ca).subject().to_string();
        assert!(rendered.contains("O=Sierra"), "{rendered}");
        assert!(rendered.contains("OU=EUD"), "{rendered}");
        assert!(
            !rendered.contains("ignored"),
            "an unknown attribute is dropped: {rendered}"
        );
    }

    #[tokio::test]
    async fn an_rsa_authority_works_too() {
        let (db, secrets, dir) = setup().await;
        let pki = PkiConfig::default();
        assert_eq!(pki.key_type, KeyType::Rsa2048, "the shipped default");

        let ca = load_or_create_root_ca(&db, &secrets, &pki, dir.path())
            .await
            .unwrap();

        assert_eq!(ca.key_type(), KeyType::Rsa2048);
        assert!(parsed(&ca).is_ca());
        assert_eq!(ca.chain_der().len(), 1, "a root is its own chain");
    }

    #[test]
    fn every_short_attribute_name_an_operator_writes_is_understood() {
        for name in ["cn", "O", "ou", "C", "l", "ST", "s"] {
            assert!(dn_type(name).is_some(), "{name}");
        }

        assert!(dn_type("EMAIL").is_none());
    }
}
