//! Shared foundation for every rustak binary (`rustak-server`,
//! `rustak-client`'s sidecar harness, `rustak-plugin-*`): config loading
//! with `${{ env.X }}` interpolation, telemetry bootstrap, a shutdown
//! primitive, the `human-errors` prelude, and credential/identity
//! primitives built on top of `rustak-api`'s newtypes.
//!
//! Every module below is an empty placeholder for M0; see
//! `.claude/plan/design/01-foundations-storage-ci.md` §2.2 for the file
//! breakdown each one grows into in a later implementation brief.

pub mod config;
pub mod errors;
pub mod identity;
pub mod prelude;
pub mod runtime;
pub mod service;
pub mod telemetry;
