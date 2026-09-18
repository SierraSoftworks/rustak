//! Who this installation knows about: accounts, channels and what it calls
//! itself.
//!
//! Thin wrappers over the repositories in [`crate::db::repos`]. They exist
//! because the rules that turn stored rows into decisions — which channels a
//! `groups` claim grants, whether a provider may adopt an existing account,
//! whether the configuration file or the setup wizard wins — have to be the
//! same wherever they are asked, and a repository is the wrong place for a rule
//! that spans three tables.
//!
//! The certificates, devices and credentials the design sketches out arrive
//! with enrolment in M2; nothing in M0 needs them yet.

pub mod groups;
pub mod settings;
pub mod users;

pub use users::VerifiedIdentity;
