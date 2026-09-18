//! The certificate the public listener presents when our own CA issues it.
//!
//! `[web.public] tls.mode = "internal"` is the default, because the deployment
//! this server is mostly for is a local network with no public DNS, no open
//! port 80 and nobody to ask for a certificate. Browsers will not trust it
//! until the CA is installed — which is what the enrolment package is for — but
//! the connection is encrypted and the device that enrolled against our CA
//! verifies it properly.
//!
//! # Why the names are part of the record
//!
//! A certificate is reissued when the names it covers change, because otherwise
//! adding a host name to the configuration would appear to work and then fail
//! at every handshake with a name mismatch nobody would think to blame on a
//! cached certificate. The stored record therefore carries the exact list it
//! was issued for, and a mismatch is a reissue rather than a warning.

use std::net::IpAddr;
use std::path::Path;

use chrono::{DateTime, Datelike as _, Utc};
use rustak_core::prelude::*;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::ca::{CaMaterial, PKI_PARTITION};
use super::keys::{KeyType, generate_key, key_pair_from_pkcs8};
use super::pem::sha256_fingerprint;
use crate::config::PkiConfig;
use crate::crypto::{Sealed, SecretContext, SecretStore};
use crate::db::{Database, KeyValueStore as _};

/// The key the server certificate's record lives under.
pub const SERVER_CERT_KEY: &str = "server_cert";

/// The record format version, so a later change can be told from this one.
const RECORD_VERSION: u8 = 1;

/// The identity the server key's sealing context is bound to.
///
/// Row identifiers start at one, so zero can never collide with a row of
/// `certificates`; the CA uses the same slot under a different context, and the
/// two are distinguished by the context's own prefix rather than by the number.
const SERVER_CERT_SLOT: CertificateId = CertificateId::new(0);

/// Advice for a failure that is ours rather than the operator's.
const ADVICE_REPORT: &[&str] =
    &["This is unexpected; please report it with the surrounding log entries."];

/// What is stored under `pki`/`server_cert`.
#[derive(Clone, Serialize, Deserialize)]
struct ServerCertRecord {
    version: u8,
    key_type: KeyType,
    /// The leaf certificate's DER encoding, base64 so it travels as JSON.
    certificate: String,
    /// The PKCS#8 private key, sealed.
    key: Sealed,
    /// Exactly what this certificate covers, so a change is noticed.
    names: Vec<String>,
    addresses: Vec<String>,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
    /// The CA that signed it: rotating the CA has to reissue the leaf.
    issuer_fingerprint: String,
}

/// A loaded server certificate, ready for rustls.
pub struct ServerCertificate {
    /// The leaf first, then the issuing CA, as a TLS handshake wants it.
    pub chain: Vec<CertificateDer<'static>>,
    /// The private key, as rustls wants it.
    pub key: PrivateKeyDer<'static>,
    /// When it stops being valid, for the renewal job M2 adds.
    pub not_after: DateTime<Utc>,
    /// The names it covers, for logging and for `/api/v1/pki/status`.
    pub names: Vec<String>,
}

impl std::fmt::Debug for ServerCertificate {
    /// Written out so the private key cannot reach a log through a stray
    /// `{:?}` on something holding one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerCertificate")
            .field("names", &self.names)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// Loads the internally issued server certificate, issuing one when there is
/// none, when it no longer covers the configured names, or when it is close
/// enough to expiry that `[pki] server_cert_renew_before` says to replace it.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when no host name is configured at all,
/// and a [`human_errors::Kind::System`] error when the record cannot be read,
/// decrypted, parsed or written.
#[instrument("pki.server_cert.load", skip_all, err(Display))]
pub async fn load_or_issue(
    db: &Database,
    secrets: &SecretStore,
    pki: &PkiConfig,
    ca: &CaMaterial,
    names: &[String],
    addresses: &[IpAddr],
) -> Result<ServerCertificate, Error> {
    let names = normalise(names);
    let addresses: Vec<String> = addresses.iter().map(IpAddr::to_string).collect();

    if names.is_empty() && addresses.is_empty() {
        return Err(human_errors::user(
            "We cannot issue this server's own certificate without knowing what host name it is reached on.",
            &[
                "Set [server] domains, or finish the setup wizard so it can record one.",
                "Alternatively set [pki] server_names, or [web.public] tls.mode to 'files'.",
            ],
        ));
    }

    let stored = db
        .get::<ServerCertRecord>(PKI_PARTITION, SERVER_CERT_KEY)
        .await?;

    if let Some(record) = stored
        && usable(&record, ca, pki, &names, &addresses)
    {
        return open(secrets, ca, record);
    }

    let record = issue(db, secrets, pki, ca, names, addresses).await?;

    open(secrets, ca, record)
}

/// Whether a stored certificate is still the one we would issue today.
fn usable(
    record: &ServerCertRecord,
    ca: &CaMaterial,
    pki: &PkiConfig,
    names: &[String],
    addresses: &[String],
) -> bool {
    record.version == RECORD_VERSION
        && record.issuer_fingerprint == ca.fingerprint()
        && record.names == names
        && record.addresses == addresses
        && record.not_after - Utc::now() > pki.server_cert_renew_before
}

/// Issues a leaf and stores it, replacing whatever was there.
async fn issue(
    db: &Database,
    secrets: &SecretStore,
    pki: &PkiConfig,
    ca: &CaMaterial,
    names: Vec<String>,
    addresses: Vec<String>,
) -> Result<ServerCertRecord, Error> {
    let key_type = pki.key_type;

    // RSA generation is hundreds of milliseconds of bignum arithmetic, which
    // would otherwise stall every task sharing this worker thread.
    let key = tokio::task::spawn_blocking(move || generate_key(key_type))
        .await
        .or_system_err(ADVICE_REPORT)??;

    let now = Utc::now();
    let expires = (now + pki.server_cert_validity).min(ca.not_after());
    let params = params(pki, &names, &addresses, now, expires)?;

    let certificate = params
        .signed_by(&key, ca.issuer())
        .or_system_err(ADVICE_REPORT)?;

    let record = ServerCertRecord {
        version: RECORD_VERSION,
        key_type,
        certificate: base64_der(certificate.der()),
        key: secrets.seal(
            &key.serialize_der(),
            SecretContext::ServerCertKey {
                certificate: SERVER_CERT_SLOT,
            },
        )?,
        names,
        addresses,
        not_before: now,
        not_after: expires,
        issuer_fingerprint: ca.fingerprint().to_string(),
    };

    db.set(PKI_PARTITION, SERVER_CERT_KEY, record.clone())
        .await?;

    info!(
        names = ?record.names,
        addresses = ?record.addresses,
        not_after = %record.not_after,
        "Issued this server's own TLS certificate from the internal authority."
    );

    Ok(record)
}

/// The parameters a server certificate is issued with.
fn params(
    pki: &PkiConfig,
    names: &[String],
    addresses: &[String],
    now: DateTime<Utc>,
    expires: DateTime<Utc>,
) -> Result<rcgen::CertificateParams, Error> {
    let mut subject_alt_names: Vec<rcgen::SanType> = Vec::new();

    for name in names {
        subject_alt_names.push(rcgen::SanType::DnsName(name.clone().try_into().map_err(
            |_| {
                human_errors::user(
                    format!("'{name}' is not a host name we can put in a certificate."),
                    &["Host names may contain letters, digits, hyphens and dots."],
                )
            },
        )?));
    }

    for address in addresses {
        let parsed: IpAddr = address.parse().or_system_err(ADVICE_REPORT)?;
        subject_alt_names.push(rcgen::SanType::IpAddress(parsed));
    }

    let mut distinguished_name = rcgen::DistinguishedName::new();
    // The common name is the canonical host, which is what a tool showing the
    // certificate puts at the top even though every modern client reads the
    // subject alternative names instead.
    distinguished_name.push(
        rcgen::DnType::CommonName,
        names.first().map(String::as_str).unwrap_or("rustak"),
    );

    for (name, value) in pki.subject_entries() {
        if let Some(kind) = dn_type(name) {
            distinguished_name.push(kind, value);
        }
    }

    let mut params = rcgen::CertificateParams::default();

    params.distinguished_name = distinguished_name;
    params.subject_alt_names = subject_alt_names;
    params.is_ca = rcgen::IsCa::NoCa;
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&random_serial()));
    params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
    params.not_after =
        rcgen::date_time_ymd(expires.year(), expires.month() as u8, expires.day() as u8);
    params.use_authority_key_identifier_extension = true;
    params.key_identifier_method = rcgen::KeyIdMethod::Sha256;

    Ok(params)
}

/// Rebuilds the handshake material from a stored record.
fn open(
    secrets: &SecretStore,
    ca: &CaMaterial,
    record: ServerCertRecord,
) -> Result<ServerCertificate, Error> {
    let der = decode_der(&record.certificate)?;
    let pkcs8 = secrets.open(
        &record.key,
        SecretContext::ServerCertKey {
            certificate: SERVER_CERT_SLOT,
        },
    )?;

    // Round-tripped through rcgen so that a record written with a key type the
    // configuration has since changed is rejected here rather than at the first
    // handshake.
    key_pair_from_pkcs8(&pkcs8, record.key_type)?;

    let leaf = CertificateDer::from(der);

    debug!(
        fingerprint = %sha256_fingerprint(&leaf),
        names = ?record.names,
        "Loaded this server's TLS certificate."
    );

    Ok(ServerCertificate {
        chain: vec![leaf, ca.certificate().clone()],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)),
        not_after: record.not_after,
        names: record.names,
    })
}

/// Lower-cased, de-duplicated, in the order they were given.
fn normalise(names: &[String]) -> Vec<String> {
    let mut seen = Vec::new();

    for name in names {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();

        if !name.is_empty() && !seen.contains(&name) {
            seen.push(name);
        }
    }

    seen
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
fn random_serial() -> [u8; 16] {
    use rand::Rng as _;

    let mut serial = [0u8; 16];
    rand::rng().fill_bytes(&mut serial);
    serial[0] &= 0x7f;

    serial
}

/// A certificate as base64, for the JSON record.
fn base64_der(der: &[u8]) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(der)
}

/// The inverse of [`base64_der`].
fn decode_der(encoded: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .or_system_err(&[
            "The stored server certificate could not be decoded; the record may be corrupt.",
        ])
}

/// Where an operator can find the certificate, for diagnostics.
pub fn server_certificate_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("pki").join("server.crt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::SecretStore;

    async fn fixtures() -> (Database, SecretStore, PkiConfig, CaMaterial) {
        let db = Database::open_in_memory().await.unwrap();
        let secrets = SecretStore::ephemeral();
        let pki = PkiConfig {
            key_type: KeyType::EcdsaP256,
            ..PkiConfig::default()
        };
        let directory = tempfile::tempdir().unwrap();
        let ca = super::super::ca::load_or_create_root_ca(&db, &secrets, &pki, directory.path())
            .await
            .unwrap();

        (db, secrets, pki, ca)
    }

    fn names() -> Vec<String> {
        vec!["TAK.example.com.".to_string(), "tak.lan".to_string()]
    }

    #[tokio::test]
    async fn a_certificate_is_issued_once_and_then_loaded() {
        let (db, secrets, pki, ca) = fixtures().await;

        let first = load_or_issue(&db, &secrets, &pki, &ca, &names(), &[])
            .await
            .unwrap();
        let second = load_or_issue(&db, &secrets, &pki, &ca, &names(), &[])
            .await
            .unwrap();

        assert_eq!(first.chain[0], second.chain[0]);
        assert_eq!(first.names, vec!["tak.example.com", "tak.lan"]);
        assert_eq!(
            first.chain.len(),
            2,
            "the leaf is presented with its issuer"
        );
    }

    #[tokio::test]
    async fn adding_a_host_name_reissues_rather_than_silently_doing_nothing() {
        // The failure this prevents is a name mismatch at every handshake,
        // which nobody would think to blame on a cached certificate.
        let (db, secrets, pki, ca) = fixtures().await;

        let first = load_or_issue(&db, &secrets, &pki, &ca, &names(), &[])
            .await
            .unwrap();

        let mut more = names();
        more.push("tak.internal".to_string());

        let second = load_or_issue(&db, &secrets, &pki, &ca, &more, &[])
            .await
            .unwrap();

        assert_ne!(first.chain[0], second.chain[0]);
        assert!(second.names.contains(&"tak.internal".to_string()));
    }

    #[tokio::test]
    async fn the_certificate_carries_the_names_and_addresses_it_was_asked_for() {
        let (db, secrets, pki, ca) = fixtures().await;
        let address: IpAddr = "192.0.2.10".parse().unwrap();

        let issued = load_or_issue(&db, &secrets, &pki, &ca, &names(), &[address])
            .await
            .unwrap();

        let (_, parsed) = x509_parser::parse_x509_certificate(&issued.chain[0]).unwrap();
        let names = parsed
            .subject_alternative_name()
            .unwrap()
            .expect("a server certificate carries subject alternative names");

        let dns: Vec<_> = names
            .value
            .general_names
            .iter()
            .filter_map(|name| match name {
                x509_parser::extensions::GeneralName::DNSName(name) => Some(*name),
                _ => None,
            })
            .collect();
        let addresses: Vec<_> = names
            .value
            .general_names
            .iter()
            .filter_map(|name| match name {
                x509_parser::extensions::GeneralName::IPAddress(bytes) => Some(bytes.to_vec()),
                _ => None,
            })
            .collect();

        assert_eq!(dns, vec!["tak.example.com", "tak.lan"]);
        assert_eq!(addresses, vec![vec![192u8, 0, 2, 10]]);
        assert!(!parsed.is_ca(), "a server certificate signs nothing");
    }

    #[tokio::test]
    async fn it_never_outlives_the_authority_that_signed_it() {
        let (db, secrets, pki, ca) = fixtures().await;
        let pki = PkiConfig {
            server_cert_validity: chrono::Duration::days(100_000),
            ..pki
        };

        let issued = load_or_issue(&db, &secrets, &pki, &ca, &names(), &[])
            .await
            .unwrap();

        assert!(issued.not_after <= ca.not_after());
    }

    #[tokio::test]
    async fn an_installation_that_knows_no_host_is_told_what_to_do_about_it() {
        let (db, secrets, pki, ca) = fixtures().await;

        let refused = load_or_issue(&db, &secrets, &pki, &ca, &[], &[])
            .await
            .unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
    }

    #[tokio::test]
    async fn a_certificate_close_to_expiry_is_replaced() {
        let (db, secrets, pki, ca) = fixtures().await;

        let first = load_or_issue(&db, &secrets, &pki, &ca, &names(), &[])
            .await
            .unwrap();

        // A renewal window longer than the certificate's whole life means every
        // load is a reissue, which is the boundary this rule turns on.
        let eager = PkiConfig {
            server_cert_renew_before: chrono::Duration::days(100_000),
            ..pki
        };

        let second = load_or_issue(&db, &secrets, &eager, &ca, &names(), &[])
            .await
            .unwrap();

        assert_ne!(first.chain[0], second.chain[0]);
    }

    #[test]
    fn host_names_are_compared_as_the_same_host_however_they_were_typed() {
        assert_eq!(
            normalise(&[
                "TAK.Example.com.".to_string(),
                " tak.example.com ".to_string(),
                String::new(),
            ]),
            vec!["tak.example.com".to_string()]
        );
    }
}
