//! Rendering and reading the textual forms certificates travel in.
//!
//! Three encodings show up on rustak's wire, and the difference between them is
//! a compatibility contract rather than a matter of taste.
//!
//! - **PEM** is the ordinary armoured form, used for `ca.crt`, for
//!   configuration files and for anything an operator copies by hand.
//! - **Bare base64** — the same body with the `-----BEGIN…` lines stripped — is
//!   what TAK Server returns from `signClient/v2`, and what node-tak
//!   (CloudTAK's client) re-armours itself. Sending it a full PEM produces a
//!   certificate with two headers, which nothing can parse.
//! - **DER** is what everything else here actually holds.
//!
//! The line wrapping matters too: TAK's own `toPEM` helper breaks at 64
//! characters and ends with a newline, and some of the Java tooling on the
//! other side is unhappy with anything else.

use base64::Engine as _;
use rustak_core::prelude::*;
use rustls_pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};

/// The PEM label a certificate carries.
const CERTIFICATE_LABEL: &str = "CERTIFICATE";

/// How many base64 characters go on a line, matching TAK's own encoder.
const LINE_WIDTH: usize = 64;

/// Base64 with the standard alphabet and padding, as PEM requires.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Encodes DER as base64 wrapped at 64 columns, with no PEM armour.
///
/// This is the body node-tak expects in `signedCert`: it adds the armour
/// itself, so anything we send with headers arrives doubled.
pub fn bare_base64_64col(der: &[u8]) -> String {
    let encoded = B64.encode(der);
    let mut wrapped = String::with_capacity(encoded.len() + encoded.len() / LINE_WIDTH + 1);

    for (index, chunk) in encoded.as_bytes().chunks(LINE_WIDTH).enumerate() {
        if index > 0 {
            wrapped.push('\n');
        }

        // Safe by construction: base64 output is ASCII, so every chunk of it
        // is a whole number of characters.
        wrapped.push_str(&String::from_utf8_lossy(chunk));
    }

    wrapped.push('\n');

    wrapped
}

/// Encodes DER as an armoured PEM certificate.
pub fn pem_certificate(der: &[u8]) -> String {
    format!(
        "-----BEGIN {CERTIFICATE_LABEL}-----\n{}-----END {CERTIFICATE_LABEL}-----\n",
        bare_base64_64col(der)
    )
}

/// Reads every certificate out of a PEM document, in the order they appear.
///
/// Anything that is not a certificate — a private key in the same file, a
/// comment block some tool added — is skipped rather than rejected, because a
/// chain file that also carries its key is a common thing for an operator to
/// paste and refusing it helps nobody.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the document is not PEM at all, or
/// when it holds no certificate.
pub fn parse_pem_chain(document: &str) -> Result<Vec<CertificateDer<'static>>, Error> {
    let blocks = pem::parse_many(document).wrap_user_err(
        "We could not read that certificate file.",
        &[
            "The file must be PEM encoded, starting with '-----BEGIN CERTIFICATE-----'.",
            "A DER encoded certificate can be converted with 'openssl x509 -inform der -in cert.der -out cert.pem'.",
        ],
    )?;

    let chain: Vec<_> = blocks
        .into_iter()
        .filter(|block| block.tag() == CERTIFICATE_LABEL)
        .map(|block| CertificateDer::from(block.into_contents()))
        .collect();

    if chain.is_empty() {
        return Err(human_errors::user(
            "That file does not contain a certificate.",
            &[
                "Check that you supplied the certificate rather than its private key or signing request.",
                "A certificate block starts with '-----BEGIN CERTIFICATE-----'.",
            ],
        ));
    }

    Ok(chain)
}

/// The SHA-256 of a certificate's DER encoding, lowercase hexadecimal.
///
/// This is rustak's identifier for a certificate: it is what the database keys
/// revocation on and what the TLS verifier looks up at handshake time. The
/// format — no colons, no uppercase — is ours; TAK stores a hash of its own and
/// never sends us one to compare against.
pub fn sha256_fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 48 bytes, so the base64 body is 64 characters: exactly one full line.
    fn der(length: usize) -> Vec<u8> {
        (0..length).map(|n| n as u8).collect()
    }

    #[test]
    fn a_bare_body_wraps_at_64_columns_and_ends_with_a_newline() {
        let encoded = bare_base64_64col(&der(100));
        let lines: Vec<_> = encoded.lines().collect();

        assert!(encoded.ends_with('\n'));
        assert!(
            !encoded.starts_with("-----"),
            "the armour is the caller's job"
        );
        assert!(lines.iter().all(|line| line.len() <= LINE_WIDTH));
        assert_eq!(lines[0].len(), LINE_WIDTH);
        assert_eq!(
            lines.concat(),
            B64.encode(der(100)),
            "wrapping must not change the body"
        );
    }

    #[test]
    fn a_body_that_is_exactly_one_line_gets_exactly_one_newline() {
        let encoded = bare_base64_64col(&der(48));

        assert_eq!(encoded.len(), LINE_WIDTH + 1);
        assert_eq!(encoded.matches('\n').count(), 1);
    }

    #[test]
    fn an_empty_body_is_still_newline_terminated() {
        assert_eq!(bare_base64_64col(&[]), "\n");
    }

    #[test]
    fn a_pem_certificate_is_the_bare_body_between_the_armour() {
        let bytes = der(100);
        let rendered = pem_certificate(&bytes);

        assert!(rendered.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(rendered.ends_with("-----END CERTIFICATE-----\n"));
        assert!(rendered.contains(&bare_base64_64col(&bytes)));
    }

    #[test]
    fn a_rendered_certificate_reads_back_as_the_same_bytes() {
        let bytes = der(300);

        let chain = parse_pem_chain(&pem_certificate(&bytes)).unwrap();

        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].as_ref(), bytes.as_slice());
    }

    #[test]
    fn a_chain_keeps_the_order_it_was_written_in() {
        let document = format!("{}{}", pem_certificate(&der(10)), pem_certificate(&der(20)));

        let chain = parse_pem_chain(&document).unwrap();

        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].as_ref(), der(10).as_slice());
        assert_eq!(chain[1].as_ref(), der(20).as_slice());
    }

    #[test]
    fn a_key_sharing_the_file_is_skipped_rather_than_refused() {
        let document = format!(
            "-----BEGIN PRIVATE KEY-----\n{}-----END PRIVATE KEY-----\n{}",
            bare_base64_64col(&der(40)),
            pem_certificate(&der(10))
        );

        let chain = parse_pem_chain(&document).unwrap();

        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].as_ref(), der(10).as_slice());
    }

    #[test]
    fn a_file_with_no_certificate_is_a_user_error() {
        for document in [
            "",
            "hello",
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        ] {
            let Err(err) = parse_pem_chain(document) else {
                panic!("{document:?} holds no certificate");
            };

            assert!(err.is(human_errors::Kind::User), "{document:?}: {err}");
        }
    }

    #[test]
    fn a_fingerprint_is_lowercase_hex_with_no_separators() {
        // The SHA-256 of the empty input, from an independent implementation.
        assert_eq!(
            sha256_fingerprint(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_fingerprint(b"x").len(), 64);
        assert_ne!(sha256_fingerprint(b"x"), sha256_fingerprint(b"y"));
    }
}
