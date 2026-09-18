//! Sidecar SDK used by every `rustak-plugin-*` binary: a TAK stream client,
//! a typed Marti API client, and a control-API client for the services
//! registry. Also used by `rustak-server`'s own integration tests as a
//! dependency-direction-respecting fake EUD.
//!
//! Two modules so far: [`sidecar`] (the [`Sidecar`](sidecar::Sidecar) trait
//! and its [`run()`](sidecar::run()) harness) and [`stream`] (the TAK CoT
//! stream client — TLS, protocol negotiation, keepalive and reconnection). The
//! Marti and control clients arrive in M2/M6 — see `.claude/plan/plan.md` →
//! Architecture → "Plugin (sidecar) contract", and `docs/plugins.md` for the
//! guide a plugin author reads.

pub mod sidecar;
pub mod stream;
