//! Sidecars: registering them, watching their health, and telling them what
//! happened.
//!
//! A rustak plugin is not loaded into this process. It is its own program with
//! its own identity, which connects to the CoT stream and calls the Marti API
//! exactly as an ATAK device does — see `docs/plugins.md`. What is *extra*, and
//! what this module is, is the small control surface that makes a fleet of them
//! operable:
//!
//! | Module | What it holds |
//! |---|---|
//! | [`auth`] | Who a control-API request is from: a service token, a client certificate, or an administrator's session |
//! | [`registry`] | What is registered, under whose account, and with what configuration |
//! | [`health`] | Heartbeats, and deciding that one is overdue |
//! | [`events`] | The `broadcast` bus behind `GET /api/v1/events` |
//!
//! The HTTP layer over all four is `web::api::services` and `web::api::events`.
//!
//! # Named `plugins`, not `services`
//!
//! `crate::services` is already the application context — the handle everything
//! in the server reaches its storage through — and two modules called `services`
//! in one crate would be a permanent `use` puzzle. The wire vocabulary stays
//! "service" (`/api/v1/services`, `users.kind = 'service'`,
//! `rustak_api::ServiceDescriptor`); only the module is called what the
//! documentation calls the thing an operator installs.
//!
//! # Nothing here can do more than a client could
//!
//! That is the whole plugin model, and it is worth restating where the code is:
//! a service token authenticates *this* API and nothing else
//! ([`Purpose::ServiceApi`](crate::identity::verify::Purpose::ServiceApi)), a
//! service's certificate is an ordinary client certificate, and its channel
//! scoping is an ordinary account's. A sidecar that floods a channel is as
//! visible, and as easy to remove, as a device that does.

pub mod auth;
pub mod events;
pub mod health;
pub mod registry;

pub use auth::Caller;
pub use events::ServerEvents;
pub use registry::RegistryError;
