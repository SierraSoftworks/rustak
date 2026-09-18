//! Symmetric keys: generation, encoding, and the fingerprint used to select
//! one without revealing it.

use std::fmt;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Generate, Key, KeyInit};
use base64::Engine;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use rustak_core::prelude::*;

/// The base64 alphabet used for every encoded value in this module.
pub(super) const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The length of an AES-256 key, in bytes.
const KEY_BYTES: usize = 32;

/// A short, non-secret fingerprint identifying which key sealed a value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId([u8; 4]);

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({self})")
    }
}

/// A 256-bit symmetric key.
///
/// The bytes are zeroed when the key is dropped, and neither [`fmt::Debug`] nor
/// any other formatting impl will render them — only the non-secret [`KeyId`].
pub struct SecretKey {
    bytes: [u8; KEY_BYTES],
}

impl SecretKey {
    /// Generates a new key from the operating system's randomness source.
    pub fn generate() -> Self {
        let key = Key::<Aes256Gcm>::generate();
        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(key.as_slice());

        Self { bytes }
    }

    /// Parses a key from its base64 or hexadecimal encoding.
    ///
    /// Both alphabets are accepted because operators paste keys from a variety
    /// of sources, and guessing wrong is a confusing failure to debug.
    pub fn from_encoded(encoded: &str) -> Result<Self, human_errors::Error> {
        let encoded = encoded.trim();

        // The config loader leaves an unresolved `${{ env.X }}` expression in
        // place verbatim rather than failing, so this is the most likely way
        // for a key to be malformed and deserves its own diagnosis.
        if encoded.contains("${{") {
            return Err(human_errors::user(
                "Your encryption key still contains an unresolved '${{ ... }}' expression.",
                &[
                    "Check that the environment variable it refers to is set in your environment or .env file.",
                    "Remember that rustak reads .env from the path given by --env, which defaults to '.env'.",
                ],
            ));
        }

        if encoded.is_empty() {
            return Err(human_errors::user(
                "Your encryption key is empty.",
                &[
                    "Set 'secret_key' under [auth] to a 32-byte key, or remove it to have one generated for you.",
                    "You can generate one with: openssl rand -base64 32",
                ],
            ));
        }

        let decoded = decode_key_bytes(encoded)?;

        if decoded.len() != KEY_BYTES {
            return Err(human_errors::user(
                format!(
                    "Your encryption key decodes to {} bytes, but AES-256 requires exactly {KEY_BYTES}.",
                    decoded.len()
                ),
                &[
                    "Generate a key of the correct length with: openssl rand -base64 32",
                    "Check that the value was not truncated when it was copied or stored.",
                ],
            ));
        }

        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(&decoded);

        Ok(Self { bytes })
    }

    /// Encodes the key for storage in configuration or a key file.
    pub fn to_encoded(&self) -> String {
        B64.encode(self.bytes)
    }

    /// A short fingerprint of the key, safe to record in envelopes and logs.
    ///
    /// This is a hash of the key rather than a slice of it, so publishing it
    /// reveals nothing about the key material.
    pub fn id(&self) -> KeyId {
        let digest = Sha256::new()
            .chain_update(b"rustak/secret-key-id/v1")
            .chain_update(self.bytes)
            .finalize();

        let mut id = [0u8; 4];
        id.copy_from_slice(&digest[..4]);

        KeyId(id)
    }

    /// Duplicates the key material.
    ///
    /// Deliberately not [`Clone`]: a store needs to hold its active key both as
    /// `active` and inside its lookup map, and this makes that one call site
    /// explicit rather than letting key material be copied anywhere a `Clone`
    /// bound would allow.
    pub(super) fn duplicate(&self) -> Self {
        Self { bytes: self.bytes }
    }

    /// The AES-256-GCM cipher this key drives.
    pub(super) fn cipher(&self) -> Aes256Gcm {
        // The byte count is fixed by the type, so this cannot fail.
        Aes256Gcm::new_from_slice(&self.bytes).expect("an AES-256 key is always 32 bytes")
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", self.id())
    }
}

/// Decodes key material from either base64 or hexadecimal.
fn decode_key_bytes(encoded: &str) -> Result<Vec<u8>, human_errors::Error> {
    // A 32-byte key is 64 hex characters. Anything that length made up purely
    // of hex digits is unambiguously hex, since base64 of 32 bytes is 43-44
    // characters.
    if encoded.len() == KEY_BYTES * 2 && encoded.bytes().all(|b| b.is_ascii_hexdigit()) {
        return hex::decode(encoded).wrap_user_err(
            "Your encryption key could not be decoded as hexadecimal.",
            &["Check that the value contains only hexadecimal digits."],
        );
    }

    // Accept both the URL-safe and standard base64 alphabets, with or without
    // padding, since `openssl rand -base64 32` emits standard-alphabet output.
    for engine in [
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(encoded.trim_end_matches('=')) {
            return Ok(decoded);
        }
    }

    Err(human_errors::user(
        "Your encryption key could not be decoded as base64 or hexadecimal.",
        &[
            "Generate a valid key with: openssl rand -base64 32",
            "Check that the value was not wrapped across lines or otherwise altered.",
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_accepted_in_the_encodings_operators_actually_paste() {
        let key = SecretKey::generate();
        let raw = B64.decode(key.to_encoded()).unwrap();

        let url_safe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
        let standard = base64::engine::general_purpose::STANDARD.encode(&raw);
        let hexadecimal = hex::encode(&raw);

        for encoded in [url_safe, standard, hexadecimal] {
            let parsed = SecretKey::from_encoded(&encoded).unwrap();
            assert_eq!(parsed.id(), key.id(), "failed for {encoded}");
        }
    }

    #[test]
    fn a_key_of_the_wrong_length_is_rejected_with_its_actual_length() {
        let short = B64.encode([0u8; 16]);
        let err = SecretKey::from_encoded(&short).unwrap_err();

        assert!(err.to_string().contains("16 bytes"), "{err}");
    }

    #[test]
    fn an_unresolved_environment_expression_is_diagnosed_specifically() {
        // The config loader leaves these in place when the variable is unset,
        // so this is the most common way a key ends up malformed.
        let err = SecretKey::from_encoded("${{ env.RUSTAK_SECRET_KEY }}").unwrap_err();

        assert!(err.to_string().contains("unresolved"), "{err}");
    }

    #[test]
    fn an_empty_key_is_diagnosed_specifically() {
        let err = SecretKey::from_encoded("   ").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn key_ids_identify_keys_without_revealing_them() {
        let key = SecretKey::generate();
        let id = key.id().to_string();

        assert_eq!(id.len(), 8);
        assert!(!key.to_encoded().contains(&id));

        // Stable across parses of the same material.
        assert_eq!(
            SecretKey::from_encoded(&key.to_encoded()).unwrap().id(),
            key.id()
        );

        assert_ne!(SecretKey::generate().id(), SecretKey::generate().id());
    }

    #[test]
    fn debug_output_never_reveals_key_material() {
        let key = SecretKey::generate();
        assert!(!format!("{key:?}").contains(&key.to_encoded()));
    }
}
