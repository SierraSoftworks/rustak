//! Encryption for the secrets rustak holds at rest.
//!
//! rustak is the certificate authority, the identity provider and the mission
//! server for every device and sidecar that connects to it, so the private
//! keys, ACME account credentials, signing keys and refresh tokens it holds
//! are as sensitive as anything a device carries. Storing them in plaintext
//! would mean that a leaked database file — or a leaked backup of one — is a
//! leak of every credential this server has ever issued or been trusted with.
//!
//! # Shape of the design
//!
//! Secrets are sealed with AES-256-GCM into a self-describing [`Sealed`]
//! envelope which is stored as an ordinary JSON value, so it travels through
//! the existing SQLite rows without any special handling.
//!
//! Two deliberate choices are worth calling out.
//!
//! **Sealing is explicit, not automatic.** It would be more ergonomic to hide
//! encryption inside a `serde` implementation so that any field could be
//! declared secret and forgotten about. We do not, because `serde` cannot
//! carry the context needed for the next property, and because an explicit
//! [`SecretStore::open`] call leaves every point where a secret is exposed
//! visible to a reader and greppable in review.
//!
//! **Every ciphertext is bound to the row that holds it.** The
//! [`SecretContext`] is passed as GCM additional authenticated data and is
//! *not* stored in the envelope — it is reconstructed from the row's own
//! identity at decryption time. An attacker who can write to the database but
//! does not hold the key therefore cannot graft one certificate's sealed
//! private key onto another certificate's row and have the server decrypt and
//! use it: the reconstructed context differs, the authentication tag fails,
//! and the open is rejected.
//!
//! # Key management
//!
//! The active key comes from configuration (and so, in practice, from an
//! environment variable or secret manager). Where none is configured we
//! generate one into a file beside the database, which keeps an existing
//! install upgrading without ceremony while still moving the key off the
//! backup path of the database itself.
//!
//! Each envelope records which key sealed it, so rotation is a matter of
//! moving the old key into `previous_secret_keys` and letting values re-seal
//! under the new key as they are written. Decryption picks the right key by
//! id rather than trying each in turn.

mod context;
mod key;
mod keyfile;
mod store;

pub use context::SecretContext;
pub use key::{KeyId, SecretKey};
pub use keyfile::{key_file_for, load_or_create_key};
pub use store::{Sealed, SecretStore};
