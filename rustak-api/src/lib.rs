//! Shared JSON contract between `rustak-server` and `rustak-ui`: DTOs for
//! the admin API, and the validated identity newtypes (`Username`,
//! `DeviceUid`, `GroupName`, typed row ids, …) that both crates share
//! without either depending on the other.
//!
//! wasm-safe: this crate depends only on `serde`/`serde_json`/`chrono`/`uuid`
//! so it can be compiled for `wasm32-unknown-unknown` by `rustak-ui` as well
//! as linked into `rustak-server`. No tokio, no tracing, no rusqlite.
//!
//! Every module below is an empty placeholder for M0; the DTOs and newtypes
//! themselves are added by a dedicated implementation brief (see
//! `.claude/plan/design/01-foundations-storage-ci.md` §2.1).

pub mod audit;
pub mod auth;
pub mod certificate;
pub mod credential;
pub mod device;
pub mod error;
pub mod group;
pub mod health;
pub mod identity;
pub mod service;
pub mod settings;
pub mod setup;
pub mod user;
