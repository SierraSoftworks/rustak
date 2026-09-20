//! Sidecar SDK used by every `rustak-plugin-*` binary: a TAK stream client, a
//! typed Marti API client, a control-API client for the services registry, and
//! the enrolment that gets a sidecar the certificate all three need. Also used
//! by `rustak-server`'s own integration tests as a dependency-direction-
//! respecting fake EUD.
//!
//! | Module | What it is |
//! |---|---|
//! | [`stream`] | The TAK CoT stream client: TLS, protocol negotiation, keepalive, reconnection |
//! | [`marti`] | A typed client for missions, files, channels and contacts |
//! | [`control`] | `/api/v1/services/*`: registration, heartbeats, per-service configuration, and the server-event feed |
//! | [`enroll`] | CSR → `signClient/v2`, which is how a sidecar gets its certificate |
//! | [`sidecar`] | The [`Sidecar`](sidecar::Sidecar) trait and the [`run()`](sidecar::run()) harness that drives one over all of the above |
//! | [`feed`] | Information feeds: an observation model, the CoT type mapping, an area of interest and a rate-limiting publisher |
//! | [`http`] | The shared `reqwest` client the two HTTP clients are built on |
//!
//! A plugin author writes against [`sidecar`] and reaches the rest through the
//! [`SidecarContext`](sidecar::SidecarContext) the harness hands it. See
//! `docs/plugins.md` for the guide, and `.claude/plan/plan.md` → Architecture →
//! "Plugin (sidecar) contract" for why a plugin is a separate process at all.

pub mod control;
pub mod enroll;
pub mod feed;
pub mod http;
pub mod marti;
pub mod sidecar;
pub mod stream;
