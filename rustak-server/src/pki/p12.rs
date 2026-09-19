//! PKCS#12 bundles, in the shapes TAK clients actually read.
//!
//! Three of them, and they are not interchangeable:
//!
//! - a **client keystore** — the private key, its certificate and the chain —
//!   which is what a manually built configuration package hands a device;
//! - a **truststore** — the authority certificates only, no key — which is what
//!   the same package hands it to verify *us* with; and
//! - the **legacy `signClient` bundle**, which the pre-v2 enrolment endpoint
//!   returns in place of JSON or XML, holding only certificates under the
//!   aliases `signedCert`, `ca0`, `ca1`…
//!
//! # Why the algorithms are the old ones by default
//!
//! ATAK's keystore is read by BouncyCastle through a JCE provider, and the
//! combination in the field reads PBES1 with 3DES and a SHA-1 MAC. Modern
//! OpenSSL will not even parse those without `-legacy`, and modern writers
//! default to AES-256 with a SHA-256 MAC, which ATAK then refuses. `p12_legacy`
//! is therefore on by default and writes what the client reads; an installation
//! whose clients are all new can turn it off.
//!
//! The passphrase has the same shape of problem: `atakatak` is what every TAK
//! client tries first, and a bundle it cannot open is a bundle nobody can use.
//! It is not a secret — the file is handed over out of band and is exactly as
//! confidential as the channel that carried it.

use p12_keystore::{
    Certificate, EncryptionAlgorithm, KeyStore, KeyStoreEntry, MacAlgorithm, PrivateKey,
    PrivateKeyChain,
};
use rustak_core::prelude::*;
use sha2::{Digest as _, Sha256};

/// The alias the enrolled certificate is stored under in a keystore, and the
/// one the legacy endpoint's bundle uses for the issued certificate.
pub const SIGNED_CERT_ALIAS: &str = "signedCert";

/// The prefix the authority certificates are stored under, numbered from zero,
/// matching the `ca0`, `ca1`… keys of the JSON enrolment response.
pub const CA_ALIAS_PREFIX: &str = "ca";

/// How many bytes of the certificate digest identify a keychain inside a
/// bundle. The value is arbitrary — it only has to match between the key bag
/// and its certificate bag — and twenty bytes is the length every other
/// producer happens to use, so tooling that prints it looks unremarkable.
const LOCAL_KEY_ID_BYTES: usize = 20;

/// Key-derivation iterations for the legacy algorithms.
///
/// 2048 is what the Java tooling in this ecosystem writes. The count barely
/// matters here: the passphrase is usually the well-known one, so the file's
/// confidentiality comes from how it is delivered, not from how expensive it
/// would be to guess a password that is not secret.
const LEGACY_ITERATIONS: u32 = 2048;

/// Iterations for the modern algorithms.
const MODERN_ITERATIONS: u32 = 10_000;

/// Advice for a failure building a bundle.
const ADVICE_REPORT: &[&str] =
    &["This is unexpected; please report it with the surrounding log entries."];

/// How a bundle is written.
#[derive(Clone, Copy)]
pub struct P12Options<'a> {
    /// The passphrase. Usually `atakatak`; see the module documentation.
    pub password: &'a str,

    /// The alias the keychain is stored under, which some clients display.
    pub friendly_name: &'a str,

    /// Whether to write PBES1 with 3DES and a SHA-1 MAC, which ATAK reads, or
    /// AES-256 with a SHA-256 MAC, which it does not.
    pub legacy: bool,
}

impl std::fmt::Debug for P12Options<'_> {
    /// Written out so the passphrase cannot reach a log through a `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("P12Options")
            .field("friendly_name", &self.friendly_name)
            .field("legacy", &self.legacy)
            .finish_non_exhaustive()
    }
}

impl<'a> P12Options<'a> {
    /// The options `[pki]` describes, for a bundle named `friendly_name`.
    pub fn from_config(pki: &'a crate::config::PkiConfig, friendly_name: &'a str) -> Self {
        Self {
            password: &pki.p12_password,
            friendly_name,
            legacy: pki.p12_legacy,
        }
    }

    /// The options for a bundle handed to one person, once, under a passphrase
    /// generated for that hand-over.
    ///
    /// Unlike [`P12Options::from_config`], `legacy` is not a setting here. The
    /// CloudTAK hand-over exists so that CloudTAK's own parser
    /// (`@tak-ps/node-p12`) can read the file, and that parser reads PBES1 with
    /// 3DES and a SHA-1 MAC and nothing else — a bundle written with the modern
    /// algorithms is one the operator cannot upload, whatever `p12_legacy`
    /// says. The confidentiality argument is the other way round from the
    /// shared `atakatak` file too: this passphrase *is* secret, it is shown
    /// once beside the download, and it is never stored.
    pub fn handover(password: &'a str, friendly_name: &'a str) -> Self {
        Self {
            password,
            friendly_name,
            legacy: true,
        }
    }
}

/// The keystore a device authenticates with: its key, its certificate, and the
/// chain up to our authority.
///
/// `key_pkcs8` is the device's own private key, which only ever exists here
/// when the server generated it — a device that sent a signing request keeps
/// its key and never gives it to us.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the key or a certificate cannot
/// be encoded.
#[instrument("pki.p12.client_keystore", skip_all, err(Display))]
pub fn client_keystore(
    key_pkcs8: &[u8],
    certificate: &[u8],
    chain: &[&[u8]],
    options: &P12Options<'_>,
) -> Result<Vec<u8>, Error> {
    let key = PrivateKey::from_der(key_pkcs8).map_err(|err| {
        human_errors::system(
            format!("The private key could not be written into a PKCS#12 bundle: {err}."),
            ADVICE_REPORT,
        )
    })?;

    let mut certificates = vec![parse(certificate)?];
    for link in chain {
        certificates.push(parse(link)?);
    }

    let mut keystore = KeyStore::new();

    keystore.add_entry(
        options.friendly_name,
        KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(
            local_key_id(certificate),
            key,
            certificates,
        )),
    );

    write(&keystore, options)
}

/// The truststore a device verifies us with: the authority chain, no key.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when a certificate cannot be encoded,
/// and a [`human_errors::Kind::User`] error when the chain is empty — a
/// truststore with nothing in it would leave the device unable to connect at
/// all, which is worse than refusing to build it.
#[instrument("pki.p12.truststore", skip_all, err(Display))]
pub fn truststore(chain: &[&[u8]], options: &P12Options<'_>) -> Result<Vec<u8>, Error> {
    if chain.is_empty() {
        return Err(human_errors::user(
            "A truststore needs at least one certificate authority.",
            &["This installation's authority should have been created at start-up; check the log."],
        ));
    }

    let mut keystore = KeyStore::new();

    for (index, certificate) in chain.iter().enumerate() {
        keystore.add_entry(
            &format!("{CA_ALIAS_PREFIX}{index}"),
            KeyStoreEntry::Certificate(parse(certificate)?),
        );
    }

    write(&keystore, options)
}

/// The bundle the pre-v2 `signClient` endpoint returns: certificates only,
/// under the aliases the clients that still call it expect.
///
/// There is no key in it. The device generated one to make the signing request
/// and pairs it with the certificate itself; TAK Server's own version of this
/// endpoint does the same.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when a certificate cannot be encoded.
#[instrument("pki.p12.legacy_signclient", skip_all, err(Display))]
pub fn legacy_signclient_v1(
    certificate: &[u8],
    chain: &[&[u8]],
    options: &P12Options<'_>,
) -> Result<Vec<u8>, Error> {
    let mut keystore = KeyStore::new();

    keystore.add_entry(
        SIGNED_CERT_ALIAS,
        KeyStoreEntry::Certificate(parse(certificate)?),
    );

    for (index, link) in chain.iter().enumerate() {
        keystore.add_entry(
            &format!("{CA_ALIAS_PREFIX}{index}"),
            KeyStoreEntry::Certificate(parse(link)?),
        );
    }

    write(&keystore, options)
}

/// Serialises a keystore with the algorithms the options ask for.
fn write(keystore: &KeyStore, options: &P12Options<'_>) -> Result<Vec<u8>, Error> {
    let writer = keystore.writer(options.password);

    let writer = if options.legacy {
        writer
            .encryption_algorithm(EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc)
            .encryption_iterations(LEGACY_ITERATIONS)
            .mac_algorithm(MacAlgorithm::HmacSha1)
            .mac_iterations(LEGACY_ITERATIONS)
    } else {
        writer
            .encryption_algorithm(EncryptionAlgorithm::PbeWithHmacSha256AndAes256)
            .encryption_iterations(MODERN_ITERATIONS)
            .mac_algorithm(MacAlgorithm::HmacSha256)
            .mac_iterations(MODERN_ITERATIONS)
    };

    writer.write().map_err(|err| {
        human_errors::system(
            format!("The PKCS#12 bundle could not be written: {err}."),
            ADVICE_REPORT,
        )
    })
}

/// Reads a DER certificate into the form the keystore holds.
fn parse(der: &[u8]) -> Result<Certificate, Error> {
    Certificate::from_der(der).map_err(|err| {
        human_errors::system(
            format!("A certificate could not be written into a PKCS#12 bundle: {err}."),
            ADVICE_REPORT,
        )
    })
}

/// The identifier tying a key bag to its certificate bag.
fn local_key_id(certificate: &[u8]) -> Vec<u8> {
    Sha256::digest(certificate)[..LOCAL_KEY_ID_BYTES].to_vec()
}

#[cfg(test)]
mod tests {
    use p12_keystore::Pkcs12ImportPolicy;

    use super::*;
    use crate::pki::keys::{KeyType, generate_key};

    struct Material {
        key: Vec<u8>,
        certificate: Vec<u8>,
        ca: Vec<u8>,
    }

    /// A real key, a real leaf and a real authority, because a PKCS#12 writer
    /// parses every one of them.
    fn material() -> Material {
        let ca_key = generate_key(KeyType::EcdsaP256).unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "rustak CA");
        let ca = ca_params.self_signed(&ca_key).unwrap();

        let key = generate_key(KeyType::EcdsaP256).unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "alice");
        let issuer = rcgen::Issuer::from_ca_cert_der(ca.der(), ca_key).unwrap();
        let certificate = params.signed_by(&key, &issuer).unwrap();

        Material {
            key: key.serialize_der(),
            certificate: certificate.der().to_vec(),
            ca: ca.der().to_vec(),
        }
    }

    fn options(legacy: bool) -> P12Options<'static> {
        P12Options {
            password: "atakatak",
            friendly_name: "alice",
            legacy,
        }
    }

    fn read(bundle: &[u8], password: &str) -> KeyStore {
        KeyStore::from_pkcs12(bundle, password, Pkcs12ImportPolicy::Relaxed)
            .expect("the bundle we wrote must read back")
    }

    #[test]
    fn a_client_keystore_round_trips_with_its_key_and_chain() {
        let material = material();
        let bundle = client_keystore(
            &material.key,
            &material.certificate,
            &[&material.ca],
            &options(true),
        )
        .unwrap();

        let store = read(&bundle, "atakatak");
        let (alias, chain) = store
            .private_key_chain()
            .expect("a client keystore holds exactly one keychain");

        assert_eq!(alias, "alice");
        assert_eq!(chain.key().as_der(), material.key);
        assert_eq!(chain.certs().len(), 2, "the leaf and the authority");
        assert_eq!(chain.certs()[0].as_der(), material.certificate);
        assert_eq!(chain.certs()[1].as_der(), material.ca);
    }

    #[test]
    fn the_leaf_comes_first_so_the_chain_reads_in_order() {
        let material = material();
        let bundle = client_keystore(
            &material.key,
            &material.certificate,
            &[&material.ca],
            &options(false),
        )
        .unwrap();

        let store = read(&bundle, "atakatak");
        let (_, chain) = store.private_key_chain().unwrap();

        assert!(chain.certs()[0].subject().contains("alice"));
        assert!(chain.certs()[1].subject().contains("rustak CA"));
    }

    #[test]
    fn a_truststore_holds_authorities_and_no_key() {
        let material = material();
        let bundle = truststore(&[&material.ca], &options(true)).unwrap();
        let store = read(&bundle, "atakatak");

        assert!(
            store.private_key_chain().is_none(),
            "no key in a truststore"
        );
        assert_eq!(store.entries_len(), 1);

        match store.entry("ca0").expect("aliases are ca0, ca1, …") {
            KeyStoreEntry::Certificate(certificate) => {
                assert_eq!(certificate.as_der(), material.ca);
            }
            other => panic!("expected a certificate, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_truststore_is_refused_rather_than_written() {
        assert!(truststore(&[], &options(true)).is_err());
    }

    #[test]
    fn the_legacy_bundle_uses_the_aliases_old_clients_look_for() {
        let material = material();
        let bundle =
            legacy_signclient_v1(&material.certificate, &[&material.ca], &options(true)).unwrap();
        let store = read(&bundle, "atakatak");

        assert_eq!(store.entries_len(), 2);
        assert!(matches!(
            store.entry(SIGNED_CERT_ALIAS),
            Some(KeyStoreEntry::Certificate(_))
        ));
        assert!(matches!(
            store.entry("ca0"),
            Some(KeyStoreEntry::Certificate(_))
        ));
        assert!(
            store.private_key_chain().is_none(),
            "the device keeps its own key"
        );
    }

    #[test]
    fn several_authorities_are_numbered_from_zero() {
        let first = material();
        let second = material();
        let bundle = legacy_signclient_v1(
            &first.certificate,
            &[&first.ca, &second.ca],
            &options(false),
        )
        .unwrap();

        let store = read(&bundle, "atakatak");

        assert!(store.entry("ca0").is_some());
        assert!(store.entry("ca1").is_some());
        assert!(store.entry("ca2").is_none());
    }

    #[test]
    fn a_legacy_bundle_declares_the_algorithms_atak_reads() {
        let material = material();
        let legacy = client_keystore(
            &material.key,
            &material.certificate,
            &[&material.ca],
            &options(true),
        )
        .unwrap();
        let modern = client_keystore(
            &material.key,
            &material.certificate,
            &[&material.ca],
            &options(false),
        )
        .unwrap();

        // The OIDs appear in the file as their DER encodings; finding them is a
        // direct check that the writer was told what we think it was told.
        //  pbeWithSHAAnd3-KeyTripleDES-CBC  1.2.840.113549.1.12.1.3
        const PBE_3DES: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x0c, 0x01, 0x03];
        //  id-sha1                          1.3.14.3.2.26
        const SHA1: &[u8] = &[0x2b, 0x0e, 0x03, 0x02, 0x1a];
        //  PBES2                            1.2.840.113549.1.5.13
        const PBES2: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d];

        assert!(contains(&legacy, PBE_3DES), "3DES for the key bag");
        assert!(contains(&legacy, SHA1), "a SHA-1 MAC");
        assert!(!contains(&legacy, PBES2));

        assert!(contains(&modern, PBES2), "PBES2 for the modern bundle");
        assert!(!contains(&modern, PBE_3DES));
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn a_bundle_cannot_be_opened_with_the_wrong_passphrase() {
        let material = material();
        let bundle = client_keystore(
            &material.key,
            &material.certificate,
            &[&material.ca],
            &P12Options {
                password: "not-the-default",
                ..options(true)
            },
        )
        .unwrap();

        assert!(
            KeyStore::from_pkcs12(&bundle, "atakatak", Pkcs12ImportPolicy::Relaxed).is_err(),
            "the passphrase must actually protect the key"
        );
        assert!(
            read(&bundle, "not-the-default")
                .private_key_chain()
                .is_some()
        );
    }

    #[test]
    fn a_certificate_that_is_not_one_is_refused() {
        let material = material();

        assert!(truststore(&[b"not a certificate"], &options(true)).is_err());
        assert!(client_keystore(b"not a key", &material.certificate, &[], &options(true)).is_err());
    }

    #[test]
    fn the_options_follow_the_configuration() {
        let pki = crate::config::PkiConfig {
            p12_password: "hunter2".to_owned(),
            p12_legacy: false,
            ..crate::config::PkiConfig::default()
        };
        let options = P12Options::from_config(&pki, "alice");

        assert_eq!(options.password, "hunter2");
        assert!(!options.legacy);
        assert!(
            !format!("{options:?}").contains("hunter2"),
            "the passphrase must not reach a log"
        );
    }

    #[test]
    fn the_key_identifier_is_derived_from_the_certificate() {
        let material = material();

        assert_eq!(
            local_key_id(&material.certificate).len(),
            LOCAL_KEY_ID_BYTES
        );
        assert_ne!(
            local_key_id(&material.certificate),
            local_key_id(&material.ca)
        );
    }

    /// `openssl pkcs12 -legacy -info` is the check an operator would run, and
    /// the one that proves the file is readable outside our own reader. It
    /// needs OpenSSL 3 with the legacy provider, so it is not a gate.
    #[test]
    #[ignore = "needs openssl 3 with the legacy provider on PATH"]
    fn openssl_reads_a_legacy_bundle() {
        use std::io::Write as _;

        let material = material();
        let bundle = client_keystore(
            &material.key,
            &material.certificate,
            &[&material.ca],
            &options(true),
        )
        .unwrap();

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bundle).unwrap();

        let output = std::process::Command::new("openssl")
            .args([
                "pkcs12",
                "-legacy",
                "-info",
                "-noout",
                "-passin",
                "pass:atakatak",
                "-in",
            ])
            .arg(file.path())
            .output()
            .expect("openssl must be on PATH for this test");

        assert!(
            output.status.success(),
            "openssl refused the bundle: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
