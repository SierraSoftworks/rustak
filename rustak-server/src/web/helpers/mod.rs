//! What more than one endpoint group needs.
//!
//! Modules directly under [`crate::web`] wire up routes and hold the trivial
//! helpers used by a single group. Anything shared — interpreting forwarded
//! request metadata, and the whole of the OpenID Connect client — lives here.

pub mod oidc;
pub mod request;
