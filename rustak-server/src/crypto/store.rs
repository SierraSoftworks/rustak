//! Sealing and opening the secrets rustak holds at rest.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, Generate, Nonce, Payload};
use base64::Engine;

use rustak_core::prelude::*;

use super::context::SecretContext;
use super::key::{B64, KeyId, SecretKey};
use super::keyfile::load_or_create_key;

/// The version stamped into new envelopes, so the format can change later
/// without leaving us unable to tell which rules an old value was written
/// under.
const ENVELOPE_VERSION: u8 = 1;

/// An encrypted value, together with the metadata needed to decrypt it.
///
/// This is stored as an ordinary JSON object, so it can live inside any row
/// the database already holds. The context the value was bound to is
/// deliberately absent: it is reconstructed from the surrounding row, which is
/// what makes relocating a ciphertext detectable.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sealed {
    /// Envelope format version.
    v: u8,

    /// The [`KeyId`] of the key this value was sealed with.
    kid: String,

    /// The per-message nonce.
    n: String,

    /// The ciphertext, with the GCM authentication tag appended.
    c: String,
}

impl fmt::Debug for Sealed {
    /// Never renders the ciphertext, so that a stray `{:?}` in a log line
    /// cannot dump the encrypted store into an observability backend.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sealed(v{}, key {})", self.v, self.kid)
    }
}

/// Seals and opens the secrets rustak holds at rest.
///
/// Obtained from `Services::secrets` (added to `crate::services` by a later
/// M0 brief) rather than a global, so that tests can supply their own key and
/// so the dependency is visible in the signature of anything that touches
/// secrets.
pub struct SecretStore {
    active: SecretKey,
    keys: HashMap<KeyId, SecretKey>,
}

impl SecretStore {
    /// Builds a store which seals with `active` and can open values sealed
    /// with `active` or any of `previous`.
    pub fn new(active: SecretKey, previous: Vec<SecretKey>) -> Self {
        let mut keys = HashMap::new();

        for key in previous {
            keys.insert(key.id(), key);
        }

        keys.insert(active.id(), active.duplicate());

        Self { active, keys }
    }

    /// Builds the store described by configuration.
    ///
    /// `secret_key` and `previous_secret_keys` are `[auth].secret_key` and
    /// `[auth].previous_secret_keys`; callers with an `AuthConfig` in hand
    /// should add a thin wrapper rather than pass the config type here (see
    /// the M0-08 status note on why this module does not depend on
    /// `crate::config`).
    ///
    /// Where no key is configured, one is generated into a file beside the
    /// database, so that an existing install starts protecting its secrets
    /// without the operator having to do anything first.
    pub fn load(
        secret_key: Option<&str>,
        previous_secret_keys: &[String],
        database_path: &Path,
    ) -> Result<Self, human_errors::Error> {
        let active = load_or_create_key(secret_key, database_path)?;

        let previous = previous_secret_keys
            .iter()
            .map(|key| {
                SecretKey::from_encoded(key).map_err(|err| {
                    human_errors::user(
                        format!("One of your retired encryption keys could not be read. {err}"),
                        &[
                            "'previous_secret_keys' under [auth] must list keys in the same format as 'secret_key'.",
                            "Remove a key from that list once no stored secret still uses it.",
                        ],
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let store = Self::new(active, previous);

        debug!(
            active_key = %store.active_key_id(),
            available_keys = store.keys.len(),
            "Loaded the secret encryption keys."
        );

        Ok(store)
    }

    /// A store backed by a freshly generated key, for tests and for the
    /// `testing` feature's in-process mocks.
    #[cfg(any(test, feature = "testing"))]
    pub fn ephemeral() -> Self {
        Self::new(SecretKey::generate(), Vec::new())
    }

    /// The id of the key new values are sealed with.
    pub fn active_key_id(&self) -> KeyId {
        self.active.id()
    }

    /// Encrypts `plaintext`, binding it to `context`.
    pub fn seal(
        &self,
        plaintext: &[u8],
        context: SecretContext<'_>,
    ) -> Result<Sealed, human_errors::Error> {
        let aad = context.to_string();
        let nonce = Nonce::<Aes256Gcm>::generate();

        let ciphertext = self
            .active
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                human_errors::system(
                    "We could not encrypt a secret before storing it.",
                    &["This is unexpected; please report it with the surrounding log entries."],
                )
            })?;

        Ok(Sealed {
            v: ENVELOPE_VERSION,
            kid: self.active.id().to_string(),
            n: B64.encode(nonce),
            c: B64.encode(ciphertext),
        })
    }

    /// Decrypts a value, requiring that it was sealed against `context`.
    pub fn open(
        &self,
        sealed: &Sealed,
        context: SecretContext<'_>,
    ) -> Result<Vec<u8>, human_errors::Error> {
        if sealed.v != ENVELOPE_VERSION {
            return Err(human_errors::system(
                format!(
                    "A stored secret uses envelope version {}, which this version of rustak does not understand.",
                    sealed.v
                ),
                &["This usually means rustak was downgraded; run the newer version instead."],
            ));
        }

        let key = self.key_for(&sealed.kid)?;

        let nonce = B64.decode(&sealed.n).wrap_system_err(
            "A stored secret has a malformed nonce and cannot be decrypted.",
            &["The record may be corrupt; removing and recreating it will resolve this."],
        )?;
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce.as_slice()).map_err(|_| {
            human_errors::system(
                "A stored secret has a nonce of the wrong length and cannot be decrypted.",
                &["The record may be corrupt; removing and recreating it will resolve this."],
            )
        })?;

        let ciphertext = B64.decode(&sealed.c).wrap_system_err(
            "A stored secret has malformed ciphertext and cannot be decrypted.",
            &["The record may be corrupt; removing and recreating it will resolve this."],
        )?;

        let aad = context.to_string();

        key.cipher()
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                // A GCM failure cannot distinguish a wrong key from a tampered
                // or relocated ciphertext, so the advice covers both.
                human_errors::system(
                    "A stored secret could not be decrypted.",
                    &[
                        "Check that your encryption key has not changed; a rotated key must be listed under 'previous_secret_keys'.",
                        "If the key is correct, the record may have been tampered with and should be recreated.",
                    ],
                )
            })
    }

    /// Encrypts a value's JSON representation.
    pub fn seal_json<T: Serialize>(
        &self,
        value: &T,
        context: SecretContext<'_>,
    ) -> Result<Sealed, human_errors::Error> {
        let plaintext = serde_json::to_vec(value).wrap_system_err(
            "We could not serialise a secret before encrypting it.",
            &["This is unexpected; please report it with the surrounding log entries."],
        )?;

        self.seal(&plaintext, context)
    }

    /// Decrypts a value and parses its JSON representation.
    pub fn open_json<T: DeserializeOwned>(
        &self,
        sealed: &Sealed,
        context: SecretContext<'_>,
    ) -> Result<T, human_errors::Error> {
        let plaintext = self.open(sealed, context)?;

        serde_json::from_slice(&plaintext).wrap_system_err(
            "A stored secret was decrypted but could not be parsed.",
            &["The record may have been written by a different version of rustak."],
        )
    }

    fn key_for(&self, kid: &str) -> Result<&SecretKey, human_errors::Error> {
        self.keys
            .iter()
            .find(|(id, _)| id.to_string() == kid)
            .map(|(_, key)| key)
            .ok_or_else(|| {
                human_errors::user(
                    format!("A stored secret was sealed with key '{kid}', which is not configured."),
                    &[
                        "Add the key that was previously in use to 'previous_secret_keys' under [auth].",
                        "If that key is lost, the affected certificates and secrets must be recreated.",
                    ],
                )
            })
    }
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SecretStore(active {}, {} key(s) available)",
            self.active.id(),
            self.keys.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ca_key_context(id: i64) -> SecretContext<'static> {
        SecretContext::CaKey {
            certificate: CertificateId::new(id),
        }
    }

    #[test]
    fn a_sealed_value_opens_again_under_the_same_context() {
        let store = SecretStore::ephemeral();
        let secret = b"ca-private-key-pem-bytes";

        let sealed = store.seal(secret, ca_key_context(1)).unwrap();
        let opened = store.open(&sealed, ca_key_context(1)).unwrap();

        assert_eq!(opened, secret);
    }

    #[test]
    fn a_sealed_values_json_never_contains_the_plaintext() {
        let store = SecretStore::ephemeral();
        let sealed = store
            .seal(b"-----BEGIN PRIVATE KEY-----very-secret", ca_key_context(1))
            .unwrap();

        let rendered = serde_json::to_string(&sealed).unwrap();
        assert!(!rendered.contains("very-secret"));
        assert!(!rendered.contains("PRIVATE KEY"));
    }

    #[test]
    fn sealing_the_same_value_twice_produces_different_ciphertext() {
        // A fresh nonce per message means an observer cannot tell that two
        // rows hold the same secret, or that a value was unchanged by an
        // update.
        let store = SecretStore::ephemeral();
        let context = ca_key_context(1);

        let first = store.seal(b"same", context.clone()).unwrap();
        let second = store.seal(b"same", context).unwrap();

        assert_ne!(first.c, second.c);
        assert_ne!(first.n, second.n);
    }

    #[test]
    fn a_ciphertext_relocated_to_another_key_will_not_open() {
        // The property the whole design exists for: an attacker with database
        // write access but no key cannot graft one certificate's sealed key
        // onto another certificate's row.
        let store = SecretStore::ephemeral();
        let sealed = store.seal(b"ca-1-private-key", ca_key_context(1)).unwrap();

        let err = store.open(&sealed, ca_key_context(2)).unwrap_err();
        assert!(err.to_string().contains("could not be decrypted"));
    }

    #[test]
    fn a_ciphertext_moved_to_another_kind_of_record_will_not_open() {
        let store = SecretStore::ephemeral();
        let sealed = store.seal(b"ca-1-private-key", ca_key_context(1)).unwrap();

        let relocated = SecretContext::ServerCertKey {
            certificate: CertificateId::new(1),
        };

        assert!(store.open(&sealed, relocated).is_err());
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let store = SecretStore::ephemeral();
        let mut sealed = store.seal(b"token", ca_key_context(1)).unwrap();

        let mut bytes = B64.decode(&sealed.c).unwrap();
        bytes[0] ^= 0x01;
        sealed.c = B64.encode(&bytes);

        assert!(store.open(&sealed, ca_key_context(1)).is_err());
    }

    #[test]
    fn a_value_sealed_with_a_retired_key_still_opens() {
        let retired = SecretKey::generate();
        let retired_id = retired.id();

        let old_store = SecretStore::new(
            SecretKey::from_encoded(&retired.to_encoded()).unwrap(),
            vec![],
        );
        let sealed = old_store.seal(b"token", ca_key_context(1)).unwrap();
        assert_eq!(sealed.kid, retired_id.to_string());

        let rotated = SecretStore::new(SecretKey::generate(), vec![retired]);
        assert_eq!(rotated.open(&sealed, ca_key_context(1)).unwrap(), b"token");

        // New values use the new key, so the store drains off the old one as
        // records are rewritten.
        let resealed = rotated.seal(b"token", ca_key_context(1)).unwrap();
        assert_eq!(resealed.kid, rotated.active_key_id().to_string());
    }

    #[test]
    fn a_value_sealed_with_an_unknown_key_explains_what_to_do() {
        let store = SecretStore::ephemeral();
        let orphan = SecretStore::ephemeral()
            .seal(b"token", ca_key_context(1))
            .unwrap();

        let err = store.open(&orphan, ca_key_context(1)).unwrap_err();
        assert!(err.to_string().contains("previous_secret_keys"), "{err}");
    }

    #[test]
    fn json_values_round_trip_through_the_envelope() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Credential {
            access_token: String,
            refresh_token: String,
        }

        let store = SecretStore::ephemeral();
        let credential = Credential {
            access_token: "at".into(),
            refresh_token: "rt".into(),
        };

        let sealed = store.seal_json(&credential, ca_key_context(1)).unwrap();
        let opened: Credential = store.open_json(&sealed, ca_key_context(1)).unwrap();

        assert_eq!(opened, credential);
    }

    #[test]
    fn envelopes_survive_a_round_trip_through_storage() {
        let store = SecretStore::ephemeral();
        let sealed = store.seal(b"token", ca_key_context(1)).unwrap();

        // Envelopes are stored as ordinary JSON values in the database, so
        // they must survive that encoding unchanged.
        let stored = serde_json::to_value(&sealed).unwrap();
        let loaded: Sealed = serde_json::from_value(stored).unwrap();

        assert_eq!(loaded, sealed);
        assert_eq!(store.open(&loaded, ca_key_context(1)).unwrap(), b"token");
    }

    #[test]
    fn debug_output_never_reveals_ciphertext_or_key_material() {
        let store = SecretStore::ephemeral();
        let sealed = store.seal(b"token", ca_key_context(1)).unwrap();

        assert!(!format!("{sealed:?}").contains(&sealed.c));
        assert!(!format!("{store:?}").contains(&store.active.to_encoded()));
    }
}
