//! Identity: the validated names, the secrets, the group rights, the principal.
//!
//! The *validated newtypes* — [`Username`], [`DeviceUid`], [`ServiceName`],
//! [`GroupName`], [`Direction`] and the typed row ids — belong to `rustak-api`,
//! which `rustak-ui` also depends on, so that a username means the same thing in
//! the browser as it does in the server without either crate depending on the
//! other. They are re-exported here, unchanged, so that server-side code has one
//! identity module to import from.
//!
//! What lives here rather than in `rustak-api` is everything that cannot be
//! compiled to wasm or must never reach a browser: [`Secret`] and its zeroizing,
//! the argon2id [`password`] functions, the [`GroupSet`] bit vectors routing
//! runs on, and [`Principal`].

pub mod groups;
pub mod password;
pub mod principal;
pub mod secret;

pub use rustak_api::identity::*;

/// What a stored, argon2id-hashed secret may be used for.
///
/// Lives in `rustak_api::credential` beside the DTOs that report it, and is
/// re-exported here because [`AuthMethod::Basic`] carries one.
pub use rustak_api::credential::CredentialKind;

pub use groups::{ANON_BITPOS, GROUP_BITS, GROUP_SET_BYTES, GroupIndex, GroupSet, can_reach};
pub use password::{
    PasswordHash, hash, hash_blocking, lookup_hint, verify, verify_blocking, verify_dummy,
    verify_dummy_blocking,
};
pub use principal::{AuthMethod, Principal, PrincipalKind};
pub use secret::{
    DEFAULT_PASSWORD_GROUPS, DEFAULT_TOKEN_BYTES, Secret, generate_password, generate_token,
};
