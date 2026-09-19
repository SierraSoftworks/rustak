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
//! [`members`] is the relation between them and [`active`] is the preference —
//! per device, then per account — that narrows it into a subscription;
//! `members` re-exports `active`, so either path names the same function.
//! [`devices`] is what has connected, and
//! [`credentials`] is what it connected with — minted, recorded and taken back
//! there, checked in [`verify`], with [`secret_cache`] keeping a re-presented
//! secret from costing an argon2 hash every time. [`sessions`] is the other
//! direction: ending what an account already has open, which taking a
//! credential or an account away has to do as well as refusing the next
//! request.
//!
//! [`cloudtak`] sits slightly apart: it is the one hand-over in which this
//! server generates a client's private key, and it is kept here — beside
//! [`credentials`], whose minting it performs — rather than under `pki` so that
//! the exception reads as something an administrator does to an account.

pub mod active;
pub mod cloudtak;
pub mod credentials;
pub mod devices;
pub mod groups;
pub mod members;
pub mod secret_cache;
pub mod sessions;
pub mod settings;
pub mod users;
pub mod verify;

pub use credentials::{MintRequest, MintedSecret};
pub use secret_cache::VerifiedSecretCache;
pub use users::VerifiedIdentity;
pub use verify::{Purpose, Verified, VerifyError};
