//! Sidecar SDK used by every `rustak-plugin-*` binary: a TAK stream client,
//! a typed Marti API client, and a control-API client for the services
//! registry. Also used by `rustak-server`'s own integration tests as a
//! dependency-direction-respecting fake EUD.
//!
//! M0 ships only the [`sidecar`] module (the `Sidecar` trait and its `run()`
//! harness); the stream/marti/control clients arrive in M1/M2/M6 — see
//! `.claude/plan/plan.md` → Architecture → "Plugin (sidecar) contract".

pub mod sidecar;
