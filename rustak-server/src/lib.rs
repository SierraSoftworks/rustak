//! rustak server library: listeners, SQLite, PKI/ACME, Marti API, OAuth2,
//! admin API, embedded UI.
//!
//! `src/main.rs` is a thin CLI entry point; this crate exposes the app
//! itself so integration tests under `tests/` can build and drive it
//! in-process. The module tree follows
//! `.claude/plan/design/01-foundations-storage-ci.md` §3.1; each module is
//! filled in by its own M0 brief.

pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
pub mod identity;
pub mod jobs;
pub mod pki;
pub mod prelude;
pub mod services;
pub mod store;
pub mod web;
