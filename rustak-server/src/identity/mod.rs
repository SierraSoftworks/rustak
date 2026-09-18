//! Who this installation knows about: accounts, devices, credentials,
//! channels, and what the server calls itself.
//!
//! Thin wrappers over the repositories in [`crate::db::repos`]. They exist
//! because the rules that turn stored rows into decisions — which channels a
//! `groups` claim grants, which endpoint a secret is accepted on, whether a
//! subscription may reach a channel, whether the configuration file or the
//! setup wizard wins — have to be the same wherever they are asked, and a
//! repository is the wrong place for a rule that spans three tables.
//!
//! # The shape of it
//!
//! [`users`] and [`groups`] are the two aggregates an administrator edits;
//! [`members`] is the relation between them, plus the per-device preference
//! that narrows it into a subscription. [`devices`] is what has connected, and
//! [`credentials`] is what it connected with — minted, recorded and taken back
//! there, checked in [`verify`], with [`secret_cache`] keeping a re-presented
//! secret from costing an argon2 hash every time.

pub mod credentials;
pub mod devices;
pub mod groups;
pub mod members;
pub mod secret_cache;
pub mod settings;
pub mod users;
pub mod verify;

pub use credentials::{MintRequest, MintedSecret};
pub use secret_cache::VerifiedSecretCache;
pub use users::VerifiedIdentity;
pub use verify::{Purpose, Verified, VerifyError};
