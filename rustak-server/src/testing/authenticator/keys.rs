//! The key pairs the software authenticator signs with, and the CBOR it writes.
//!
//! One key per algorithm per process: RSA generation would otherwise dominate
//! the suite, and which key signs is not what any of these tests is about.
//!
//! # Why all three algorithms are here
//!
//! Registration offers ES256, RS256 and EdDSA, so all three are what a browser
//! may come back with — and an allow-list that is never exercised is an
//! allow-list nobody knows works. [`Algorithm::Unsupported`] is the other half
//! of that: a COSE key whose algorithm was never offered, which the relying
//! party has to refuse rather than puzzle over.

use std::sync::LazyLock;

use ciborium::Value;
use rsa::RsaPrivateKey;
use rsa::traits::PublicKeyParts as _;

/// The COSE key type for an octet key pair (Ed25519).
const COSE_KTY_OKP: i64 = 1;

/// The COSE key type for a NIST curve.
const COSE_KTY_EC2: i64 = 2;

/// The COSE key type for RSA.
const COSE_KTY_RSA: i64 = 3;

/// The COSE curve identifier for Ed25519.
const COSE_CRV_ED25519: i64 = 6;

/// The COSE curve identifier for P-256.
const COSE_CRV_P256: i64 = 1;

/// One process-wide RSA key.
static RSA_KEY: LazyLock<RsaPrivateKey> = LazyLock::new(|| {
    RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .expect("generate an RSA key for the authenticator under test")
});

/// One process-wide P-256 key.
static P256_KEY: LazyLock<p256::ecdsa::SigningKey> =
    LazyLock::new(|| p256::ecdsa::SigningKey::from_slice(&seed()).expect("a P-256 scalar"));

/// One process-wide Ed25519 key.
static ED25519_KEY: LazyLock<ed25519_dalek::SigningKey> =
    LazyLock::new(|| ed25519_dalek::SigningKey::from_bytes(&seed()));

/// Thirty-two random bytes, from the workspace's own `rand`.
///
/// `p256` and `ed25519-dalek` each want a `rand_core` 0.6 source and the
/// workspace is on a later `rand`, so the seed is drawn here and handed over as
/// bytes rather than dragging a second random-number crate in for two keys.
fn seed() -> [u8; 32] {
    use rand::Rng as _;

    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);

    bytes
}

/// What a credential signs with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Algorithm {
    /// ECDSA over P-256 with SHA-256, COSE `-7`. What every platform
    /// authenticator actually produces, so it is the default.
    #[default]
    Es256,
    /// RSASSA-PKCS1-v1_5 with SHA-256, COSE `-257`.
    Rs256,
    /// Ed25519, COSE `-8`.
    Ed25519,
    /// A COSE key labelled with an algorithm the relying party never offered.
    ///
    /// The key material is a real P-256 key, so the only thing wrong with it is
    /// the identifier — which is exactly the case an allow-list exists for.
    Unsupported,
}

impl Algorithm {
    /// The COSE identifier written into the credential's public key.
    const fn cose_id(self) -> i64 {
        match self {
            Self::Es256 => -7,
            Self::Ed25519 => -8,
            Self::Rs256 => -257,
            // Not assigned to anything, and deliberately so.
            Self::Unsupported => -65_535,
        }
    }

    /// The credential's public key, as CTAP2 canonical CBOR.
    pub fn cose_key(self) -> Vec<u8> {
        let entries = match self {
            Self::Es256 | Self::Unsupported => {
                let point = P256_KEY.verifying_key().to_encoded_point(false);
                vec![
                    (1, Value::from(COSE_KTY_EC2)),
                    (3, Value::from(self.cose_id())),
                    (-1, Value::from(COSE_CRV_P256)),
                    (
                        -2,
                        Value::Bytes(point.x().expect("an x coordinate").to_vec()),
                    ),
                    (
                        -3,
                        Value::Bytes(point.y().expect("a y coordinate").to_vec()),
                    ),
                ]
            }
            Self::Ed25519 => vec![
                (1, Value::from(COSE_KTY_OKP)),
                (3, Value::from(self.cose_id())),
                (-1, Value::from(COSE_CRV_ED25519)),
                (
                    -2,
                    Value::Bytes(ED25519_KEY.verifying_key().to_bytes().to_vec()),
                ),
            ],
            Self::Rs256 => {
                let public = RSA_KEY.to_public_key();
                vec![
                    (1, Value::from(COSE_KTY_RSA)),
                    (3, Value::from(self.cose_id())),
                    (-1, Value::Bytes(public.n().to_bytes_be())),
                    (-2, Value::Bytes(public.e().to_bytes_be())),
                ]
            }
        };

        // CTAP2 canonical CBOR orders map keys by encoded length and then
        // bytewise. Every key here encodes to one byte, and `0x01 < 0x03 <
        // 0x20 < 0x21 < 0x22`, so the order written above is already the
        // canonical one — which is asserted below rather than assumed.
        encode(&Value::Map(
            entries
                .into_iter()
                .map(|(label, value)| (Value::from(label), value))
                .collect(),
        ))
    }

    /// Signs the authenticator data and the client-data hash.
    pub fn sign(self, message: &[u8]) -> Vec<u8> {
        use sha2::{Digest as _, Sha256};

        match self {
            Self::Es256 | Self::Unsupported => {
                use p256::ecdsa::signature::Signer as _;

                let signature: p256::ecdsa::Signature = P256_KEY.sign(message);

                signature.to_der().as_bytes().to_vec()
            }
            Self::Ed25519 => {
                use ed25519_dalek::Signer as _;

                ED25519_KEY.sign(message).to_bytes().to_vec()
            }
            // `rsa` 0.9 is on `digest` 0.10 while the workspace's `sha2` is on
            // 0.11, so the padding scheme is told about `rsa`'s own `Sha256`
            // (which only decides the DigestInfo prefix) and handed the bytes
            // the workspace's produced.
            Self::Rs256 => RSA_KEY
                .sign(
                    rsa::Pkcs1v15Sign::new::<rsa::sha2::Sha256>(),
                    Sha256::digest(message).as_slice(),
                )
                .expect("sign the assertion"),
        }
    }
}

/// The attestation object, in the `none` format.
///
/// CTAP2 canonical CBOR again: `"fmt"`, `"attStmt"` and `"authData"` are three,
/// seven and eight bytes long, so that is the order they go in.
pub fn attestation_object(auth_data: &[u8]) -> Vec<u8> {
    encode(&Value::Map(vec![
        (Value::Text("fmt".into()), Value::Text("none".into())),
        (Value::Text("attStmt".into()), Value::Map(Vec::new())),
        (
            Value::Text("authData".into()),
            Value::Bytes(auth_data.to_vec()),
        ),
    ]))
}

/// Renders a CBOR value.
fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).expect("render CBOR");

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cose_key_is_in_ctap2_canonical_order() {
        // The relying party's CBOR decoder refuses a map whose keys are not in
        // canonical order, so an authenticator that wrote them any other way
        // would be testing the wrong failure.
        for algorithm in [
            Algorithm::Es256,
            Algorithm::Rs256,
            Algorithm::Ed25519,
            Algorithm::Unsupported,
        ] {
            let encoded = algorithm.cose_key();
            let value: Value = ciborium::from_reader(encoded.as_slice()).expect("valid CBOR");
            let Value::Map(entries) = value else {
                panic!("a COSE key is a map");
            };

            let keys: Vec<Vec<u8>> = entries.iter().map(|(key, _)| encode(key)).collect();
            let mut sorted = keys.clone();
            sorted.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));

            assert_eq!(keys, sorted, "{algorithm:?} is not canonically ordered");
        }
    }

    #[test]
    fn an_unsupported_algorithm_differs_from_the_offered_ones_by_its_label_alone() {
        assert_eq!(Algorithm::Unsupported.cose_id(), -65_535);
        assert_ne!(
            Algorithm::Unsupported.cose_key(),
            Algorithm::Es256.cose_key(),
        );
    }
}
