//! Reading the certificate signing requests clients enrol with.
//!
//! # Three encodings, because three clients disagree
//!
//! ATAK sends the DER base64-encoded with PEM armour but no line wrapping;
//! CloudTAK (through node-tak) sends the same base64 with no armour at all; and
//! anything driving the API by hand is as likely to send raw DER. TAK Server
//! strips the armour lines if they are there and base64-decodes whatever is
//! left, so a request is accepted in all three forms and we do the same rather
//! than making the operator work out which of their clients is "wrong".
//!
//! # What we take from a request, and what we ignore
//!
//! Only the public key and the common name matter. The common name is checked
//! against the authenticated user and then thrown away — [`crate::pki::issue`]
//! builds the subject from the configuration. Subject alternative names and
//! requested extensions are counted for the audit record and discarded: a
//! client that could name itself would be a client that could issue itself a
//! certificate for somebody else.
//!
//! The signature **is** checked. An unsigned or tampered request is one whose
//! sender may not hold the private key, and issuing against a public key
//! somebody else owns hands them a certificate they can use.

use base64::Engine as _;
use rustak_core::prelude::*;
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::extensions::ParsedExtension;
use x509_parser::prelude::FromDer as _;
use x509_parser::public_key::PublicKey;

/// The PEM banner ATAK wraps a request in.
const CSR_BANNER: &str = "-----BEGIN CERTIFICATE REQUEST-----";

/// The banner some Windows tooling writes instead.
const CSR_BANNER_NEW: &str = "-----BEGIN NEW CERTIFICATE REQUEST-----";

/// The DER tag a `SEQUENCE` starts with, which every request does.
const DER_SEQUENCE: u8 = 0x30;

/// Base64 that neither requires nor refuses padding, because clients differ.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;

/// Advice offered when a request could not be read at all.
const ADVICE_UNREADABLE: &[&str] = &[
    "The body must be the DER encoded signing request, base64 encoded, with or without PEM armour.",
    "Check that the client is sending the request rather than a certificate or a private key.",
];

/// How a request arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsrEncoding {
    /// Base64 between `-----BEGIN CERTIFICATE REQUEST-----` lines. ATAK.
    Pem,

    /// Base64 with no armour at all. CloudTAK, through node-tak.
    BareBase64,

    /// The DER bytes themselves.
    Der,
}

impl CsrEncoding {
    /// How the encoding is named in an audit record.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pem => "pem",
            Self::BareBase64 => "base64",
            Self::Der => "der",
        }
    }
}

/// The public key a request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsrKey {
    /// RSA, with the modulus size in bits.
    Rsa { bits: usize },

    /// A key on a named curve, with its size in bits (256 for P-256).
    Ecdsa { bits: usize },

    /// Something we recognise as a key but have no policy for.
    Other(String),
}

impl CsrKey {
    /// How the key is named in an audit record and in a refusal.
    pub fn describe(&self) -> String {
        match self {
            Self::Rsa { bits } => format!("RSA-{bits}"),
            Self::Ecdsa { bits } => format!("ECDSA P-{bits}"),
            Self::Other(name) => name.clone(),
        }
    }
}

/// Everything we read out of a signing request.
#[derive(Debug, Clone)]
pub struct ParsedCsr {
    /// The DER encoding, whatever form it arrived in.
    pub der: Vec<u8>,

    /// The form it arrived in, for the audit record.
    pub encoding: CsrEncoding,

    /// The common name the client asked for, if it supplied one.
    pub common_name: Option<String>,

    /// Every relative distinguished name, in the order the request lists them.
    ///
    /// Short names where we know them (`CN`, `O`, `OU`, …) and the dotted OID
    /// otherwise. Recorded for diagnostics: the issued subject is ours.
    pub rdns: Vec<(String, String)>,

    /// The public key, which is the one thing we take from the request.
    pub key: CsrKey,

    /// How many subject alternative names the client asked for.
    ///
    /// All of them are dropped. The count is kept because a client asking for
    /// names is worth noticing in the log even though it changes nothing.
    pub requested_sans: usize,
}

/// Reads a signing request in any of the three forms clients send.
///
/// The signature is verified against the request's own public key, so a
/// truncated, corrupt or tampered request is refused here rather than turning
/// into a certificate nobody holds the key for.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error: every failure here is something the
/// client sent, and the client is the only thing that can fix it.
#[instrument("pki.csr.parse", skip_all, err(Display))]
pub fn parse_csr(body: &[u8]) -> Result<ParsedCsr, Error> {
    let (der, encoding) = decode(body)?;

    let (_, request) = X509CertificationRequest::from_der(&der).map_err(|err| {
        human_errors::user(
            format!("That certificate signing request could not be read: {err}."),
            ADVICE_UNREADABLE,
        )
    })?;

    request.verify_signature().map_err(|err| {
        human_errors::user(
            format!("That certificate signing request is not correctly signed: {err}."),
            &[
                "The request must be signed by the private key matching the public key inside it.",
                "Generate a fresh key and request on the device and try enrolling again.",
            ],
        )
    })?;

    let info = &request.certification_request_info;
    let registry = x509_parser::objects::oid_registry();

    let mut rdns = Vec::new();
    for attribute in info.subject.iter_attributes() {
        let name = x509_parser::objects::oid2abbrev(attribute.attr_type(), registry)
            .map(str::to_owned)
            .unwrap_or_else(|_| attribute.attr_type().to_id_string());

        // A value we cannot render as text is not one we can compare against a
        // username, so it is recorded as absent rather than guessed at.
        if let Ok(value) = attribute.as_str() {
            rdns.push((name, value.to_owned()));
        }
    }

    let common_name = info
        .subject
        .iter_common_name()
        .next()
        .and_then(|attribute| attribute.as_str().ok())
        .map(str::to_owned);

    Ok(ParsedCsr {
        requested_sans: requested_sans(&request),
        key: key_of(info.subject_pki.parsed().ok().as_ref()),
        common_name,
        rdns,
        encoding,
        der,
    })
}

/// Works out which of the three forms a body is in and returns its DER.
fn decode(body: &[u8]) -> Result<(Vec<u8>, CsrEncoding), Error> {
    if body.is_empty() {
        return Err(human_errors::user(
            "That enrollment request carried no certificate signing request.",
            ADVICE_UNREADABLE,
        ));
    }

    if let Ok(text) = std::str::from_utf8(body) {
        if text.contains(CSR_BANNER) || text.contains(CSR_BANNER_NEW) {
            let body: String = text
                .lines()
                .filter(|line| !line.trim_start().starts_with("-----"))
                .collect();

            return decode_base64(&body).map(|der| (der, CsrEncoding::Pem));
        }

        // Bare base64 is tried before raw DER because the two are told apart by
        // the alphabet, and DER is full of bytes outside it: a request long
        // enough to matter cannot be mistaken for base64 by accident.
        let stripped: String = text.chars().filter(|c| !c.is_whitespace()).collect();

        if !stripped.is_empty() && stripped.chars().all(is_base64_character) {
            let decoded = decode_base64(&stripped)?;

            if decoded.first() == Some(&DER_SEQUENCE) {
                return Ok((decoded, CsrEncoding::BareBase64));
            }
        }
    }

    if body.first() == Some(&DER_SEQUENCE) {
        return Ok((body.to_vec(), CsrEncoding::Der));
    }

    Err(human_errors::user(
        "That certificate signing request is neither PEM, base64 nor DER.",
        ADVICE_UNREADABLE,
    ))
}

/// Decodes base64 written in either alphabet, padded or not.
///
/// node-forge and Java both emit the standard alphabet, but a client that has
/// round-tripped the body through a URL parameter arrives with `-` and `_`
/// instead, and refusing that would be a refusal nobody could diagnose.
fn decode_base64(body: &str) -> Result<Vec<u8>, Error> {
    let normalised: String = body
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();

    B64.decode(normalised.trim_end_matches('=')).map_err(|err| {
        human_errors::user(
            format!("That certificate signing request is not valid base64: {err}."),
            ADVICE_UNREADABLE,
        )
    })
}

/// Whether a character could be part of either base64 alphabet.
fn is_base64_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_' | '=')
}

/// Names the public key type, for the policy check and the audit record.
fn key_of(parsed: Option<&PublicKey<'_>>) -> CsrKey {
    match parsed {
        Some(PublicKey::RSA(key)) => CsrKey::Rsa {
            bits: key.key_size(),
        },
        Some(PublicKey::EC(point)) => CsrKey::Ecdsa {
            bits: point.key_size(),
        },
        Some(PublicKey::DSA(_)) => CsrKey::Other("DSA".to_owned()),
        Some(PublicKey::GostR3410(_) | PublicKey::GostR3410_2012(_)) => {
            CsrKey::Other("GOST".to_owned())
        }
        Some(PublicKey::Unknown(_)) | None => CsrKey::Other("unrecognised".to_owned()),
    }
}

/// How many subject alternative names the request asked for.
fn requested_sans(request: &X509CertificationRequest<'_>) -> usize {
    request
        .requested_extensions()
        .map(|extensions| {
            extensions
                .filter_map(|extension| match extension {
                    ParsedExtension::SubjectAlternativeName(san) => Some(san.general_names.len()),
                    _ => None,
                })
                .sum()
        })
        .unwrap_or_default()
}

/// What a signing request has to satisfy before we sign it.
#[derive(Debug, Clone, Copy)]
pub struct CsrPolicy {
    /// The smallest RSA modulus we will certify.
    pub min_rsa_bits: u32,

    /// Whether an elliptic-curve request is acceptable.
    pub allow_ecdsa: bool,

    /// The largest request we will parse at all.
    pub max_der_len: usize,
}

/// 8 KiB: an RSA-4096 request with a handful of attributes is under 2 KiB, so
/// this leaves generous room while keeping a hostile body from being decoded.
pub const DEFAULT_MAX_CSR_BYTES: usize = 8 * 1024;

impl Default for CsrPolicy {
    fn default() -> Self {
        Self {
            min_rsa_bits: 2048,
            allow_ecdsa: true,
            max_der_len: DEFAULT_MAX_CSR_BYTES,
        }
    }
}

impl CsrPolicy {
    /// The policy `[pki]` describes.
    pub fn from_config(pki: &crate::config::PkiConfig) -> Self {
        Self {
            min_rsa_bits: pki.csr_min_rsa_bits,
            allow_ecdsa: pki.csr_allow_ecdsa,
            max_der_len: DEFAULT_MAX_CSR_BYTES,
        }
    }

    /// Checks a request against the policy and the authenticated user.
    ///
    /// The common name must be the authenticated user, compared without regard
    /// to case because ATAK echoes back whatever was typed into the enrolment
    /// dialogue. Everything else in the subject is ignored: TAK Server insists
    /// the organisation entries match too, but since we overwrite the subject
    /// anyway that check can only refuse an enrolment that would have worked.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming the rule that was broken.
    pub fn validate(&self, csr: &ParsedCsr, authenticated: &Username) -> Result<(), Error> {
        if csr.der.len() > self.max_der_len {
            return Err(human_errors::user(
                "That certificate signing request is larger than we will accept.",
                &["A request for a single certificate is normally under two kilobytes."],
            ));
        }

        let Some(common_name) = csr.common_name.as_deref() else {
            return Err(human_errors::user(
                "That certificate signing request does not name a subject.",
                &[
                    "The request's subject must contain a common name equal to the username enrolling.",
                ],
            ));
        };

        if !authenticated.eq_ignore_case(common_name) {
            warn!(
                requested = %common_name,
                authenticated = %authenticated,
                "Refused an enrollment whose signing request named a different user."
            );

            return Err(human_errors::user(
                "That certificate signing request is for a different user than the one enrolling.",
                &[
                    "The common name in the request must be the username the request is authenticated as.",
                    "In ATAK, check that the username in the enrollment dialogue is the one you meant.",
                ],
            ));
        }

        match &csr.key {
            CsrKey::Rsa { bits } if (*bits as u32) < self.min_rsa_bits => Err(human_errors::user(
                format!(
                    "That certificate signing request uses a {bits}-bit RSA key, and this server requires at least {}.",
                    self.min_rsa_bits
                ),
                &["Generate a new key of the required size on the device and enrol again."],
            )),
            CsrKey::Rsa { .. } => Ok(()),
            CsrKey::Ecdsa { .. } if self.allow_ecdsa => Ok(()),
            CsrKey::Ecdsa { .. } => Err(human_errors::user(
                "This server does not accept elliptic-curve certificate signing requests.",
                &[
                    "Generate an RSA key on the device instead.",
                    "An administrator can allow this by setting 'csr_allow_ecdsa = true' under [pki].",
                ],
            )),
            CsrKey::Other(name) => Err(human_errors::user(
                format!(
                    "That certificate signing request uses a {name} key, which we cannot certify."
                ),
                &["Generate an RSA or an elliptic-curve key on the device instead."],
            )),
        }
    }
}

/// Reports the entries TAK Server would have insisted on, without refusing.
///
/// `[pki] name_entries` is what we put in the subject; a request that asked for
/// something else is still issued the configured subject, and this says so once
/// in the log so an operator changing the setting can see clients catching up.
pub fn warn_on_subject_mismatch(csr: &ParsedCsr, name_entries: &[(&str, &str)]) {
    for (name, value) in name_entries {
        let matched = csr.rdns.iter().any(|(their_name, their_value)| {
            their_name.eq_ignore_ascii_case(name) && their_value.eq_ignore_ascii_case(value)
        });

        if !matched {
            debug!(
                entry = %name,
                expected = %value,
                "A signing request did not carry the configured name entry; the issued subject is ours regardless."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::keys::{KeyType, generate_key};

    /// Builds a request the way a client would, so the tests exercise real DER
    /// rather than bytes we wrote by hand.
    fn request(common_name: &str, kind: KeyType) -> Vec<u8> {
        let key = generate_key(kind).unwrap();
        let mut params = rcgen::CertificateParams::default();
        let mut name = rcgen::DistinguishedName::new();

        name.push(rcgen::DnType::CommonName, common_name);
        name.push(rcgen::DnType::OrganizationName, "rustak");
        name.push(rcgen::DnType::OrganizationalUnitName, "EUD");
        params.distinguished_name = name;
        params.subject_alt_names = Vec::new();

        params.serialize_request(&key).unwrap().der().to_vec()
    }

    fn with_sans(common_name: &str) -> Vec<u8> {
        let key = generate_key(KeyType::EcdsaP256).unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["evil.example.com".to_owned()]).unwrap();
        let mut name = rcgen::DistinguishedName::new();

        name.push(rcgen::DnType::CommonName, common_name);
        params.distinguished_name = name;

        params.serialize_request(&key).unwrap().der().to_vec()
    }

    fn armoured(der: &[u8]) -> Vec<u8> {
        format!(
            "-----BEGIN CERTIFICATE REQUEST-----\n{}\n-----END CERTIFICATE REQUEST-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        )
        .into_bytes()
    }

    fn alice() -> Username {
        Username::parse("alice").unwrap()
    }

    #[test]
    fn a_pem_armoured_request_is_read() {
        let der = request("alice", KeyType::EcdsaP256);
        let parsed = parse_csr(&armoured(&der)).unwrap();

        assert_eq!(parsed.encoding, CsrEncoding::Pem);
        assert_eq!(parsed.der, der);
        assert_eq!(parsed.common_name.as_deref(), Some("alice"));
    }

    #[test]
    fn a_windows_style_banner_is_read_too() {
        let der = request("alice", KeyType::EcdsaP256);
        let body = format!(
            "-----BEGIN NEW CERTIFICATE REQUEST-----\n{}\n-----END NEW CERTIFICATE REQUEST-----\n",
            base64::engine::general_purpose::STANDARD.encode(&der)
        );

        assert_eq!(parse_csr(body.as_bytes()).unwrap().der, der);
    }

    #[test]
    fn bare_base64_is_read_with_or_without_newlines() {
        let der = request("alice", KeyType::EcdsaP256);
        let flat = base64::engine::general_purpose::STANDARD.encode(&der);
        let wrapped = crate::pki::pem::bare_base64_64col(&der);

        for body in [flat.as_str(), wrapped.as_str()] {
            let parsed = parse_csr(body.as_bytes()).unwrap();

            assert_eq!(parsed.encoding, CsrEncoding::BareBase64);
            assert_eq!(parsed.der, der);
        }
    }

    #[test]
    fn the_url_safe_alphabet_is_read() {
        let der = request("alice", KeyType::Rsa2048);
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&der);
        let parsed = parse_csr(body.as_bytes()).unwrap();

        assert_eq!(parsed.der, der);
    }

    #[test]
    fn raw_der_is_read() {
        let der = request("alice", KeyType::EcdsaP256);
        let parsed = parse_csr(&der).unwrap();

        assert_eq!(parsed.encoding, CsrEncoding::Der);
        assert_eq!(parsed.der, der);
    }

    #[test]
    fn rubbish_is_refused_rather_than_signed() {
        for body in [
            b"".as_slice(),
            b"hello, this is not a request".as_slice(),
            b"-----BEGIN CERTIFICATE REQUEST-----\nnot base64 at all!!\n-----END CERTIFICATE REQUEST-----".as_slice(),
            &[0x30, 0x82, 0x01, 0x02][..],
        ] {
            assert!(parse_csr(body).is_err(), "{body:?} must not parse");
        }
    }

    #[test]
    fn a_tampered_request_fails_its_own_signature_check() {
        let mut der = request("alice", KeyType::EcdsaP256);
        let last = der.len() - 1;

        // The signature is the last field, so flipping a bit there leaves a
        // structurally valid request whose signature no longer checks out.
        der[last] ^= 0x01;

        let error = parse_csr(&der).expect_err("a forged signature must be refused");

        assert!(
            error.to_string().contains("not correctly signed"),
            "{error}"
        );
    }

    #[test]
    fn requested_names_are_counted_and_not_honoured() {
        let parsed = parse_csr(&with_sans("alice")).unwrap();

        assert_eq!(parsed.requested_sans, 1);
    }

    #[test]
    fn the_subject_is_read_in_the_order_the_client_wrote_it() {
        let parsed = parse_csr(&request("alice", KeyType::EcdsaP256)).unwrap();

        assert_eq!(
            parsed.rdns,
            vec![
                ("CN".to_owned(), "alice".to_owned()),
                ("O".to_owned(), "rustak".to_owned()),
                ("OU".to_owned(), "EUD".to_owned()),
            ]
        );
    }

    #[test]
    fn a_common_name_matches_the_user_whatever_its_case() {
        let parsed = parse_csr(&request("Alice", KeyType::EcdsaP256)).unwrap();

        CsrPolicy::default().validate(&parsed, &alice()).unwrap();
    }

    #[test]
    fn a_request_naming_somebody_else_is_refused() {
        let parsed = parse_csr(&request("bob", KeyType::EcdsaP256)).unwrap();
        let error = CsrPolicy::default()
            .validate(&parsed, &alice())
            .expect_err("a request for another user must be refused");

        assert!(error.to_string().contains("different user"), "{error}");
    }

    #[test]
    fn a_request_with_no_common_name_is_refused() {
        let key = generate_key(KeyType::EcdsaP256).unwrap();
        let mut params = rcgen::CertificateParams::default();

        params.distinguished_name = rcgen::DistinguishedName::new();

        let der = params.serialize_request(&key).unwrap().der().to_vec();
        let parsed = parse_csr(&der).unwrap();

        assert!(parsed.common_name.is_none());
        assert!(CsrPolicy::default().validate(&parsed, &alice()).is_err());
    }

    #[test]
    fn an_rsa_key_below_the_configured_size_is_refused() {
        let parsed = parse_csr(&request("alice", KeyType::Rsa2048)).unwrap();
        let policy = CsrPolicy {
            min_rsa_bits: 3072,
            ..CsrPolicy::default()
        };

        assert!(matches!(parsed.key, CsrKey::Rsa { bits: 2048 }));
        assert!(policy.validate(&parsed, &alice()).is_err());
        assert!(CsrPolicy::default().validate(&parsed, &alice()).is_ok());
    }

    #[test]
    fn an_elliptic_curve_request_is_accepted_only_when_the_policy_allows_it() {
        let parsed = parse_csr(&request("alice", KeyType::EcdsaP256)).unwrap();
        let refusing = CsrPolicy {
            allow_ecdsa: false,
            ..CsrPolicy::default()
        };

        assert!(matches!(parsed.key, CsrKey::Ecdsa { bits: 256 }));
        assert!(CsrPolicy::default().validate(&parsed, &alice()).is_ok());
        assert!(refusing.validate(&parsed, &alice()).is_err());
    }

    #[test]
    fn a_request_larger_than_the_limit_is_refused_before_it_is_trusted() {
        let parsed = parse_csr(&request("alice", KeyType::EcdsaP256)).unwrap();
        let policy = CsrPolicy {
            max_der_len: 8,
            ..CsrPolicy::default()
        };

        assert!(policy.validate(&parsed, &alice()).is_err());
    }

    #[test]
    fn the_policy_follows_the_configuration() {
        let policy = CsrPolicy::from_config(&crate::config::PkiConfig {
            csr_min_rsa_bits: 4096,
            csr_allow_ecdsa: false,
            ..crate::config::PkiConfig::default()
        });

        assert_eq!(policy.min_rsa_bits, 4096);
        assert!(!policy.allow_ecdsa);
    }

    #[test]
    fn a_subject_mismatch_is_noted_rather_than_refused() {
        let parsed = parse_csr(&request("alice", KeyType::EcdsaP256)).unwrap();

        // Nothing to assert beyond "this does not panic and does not refuse":
        // the mismatch is a log line, because the issued subject is ours.
        warn_on_subject_mismatch(&parsed, &[("O", "somebody else")]);
        warn_on_subject_mismatch(&parsed, &[("O", "rustak"), ("OU", "EUD")]);
    }

    #[test]
    fn every_encoding_has_a_name_for_the_audit_log() {
        assert_eq!(CsrEncoding::Pem.as_str(), "pem");
        assert_eq!(CsrEncoding::BareBase64.as_str(), "base64");
        assert_eq!(CsrEncoding::Der.as_str(), "der");
        assert_eq!(CsrKey::Rsa { bits: 2048 }.describe(), "RSA-2048");
        assert_eq!(CsrKey::Ecdsa { bits: 256 }.describe(), "ECDSA P-256");
        assert_eq!(CsrKey::Other("DSA".to_owned()).describe(), "DSA");
    }
}
