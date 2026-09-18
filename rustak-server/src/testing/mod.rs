//! What the tests build the server out of.
//!
//! Compiled under `cfg(test)` and behind the `testing` feature, so the
//! in-process integration suites under `tests/` can reach the same helpers the
//! unit tests use rather than growing a second, drifting copy.
//!
//! # Why the identity provider and the authenticator are real
//!
//! The cheap way to test a sign-in is a branch in the production path: accept a
//! symmetric signature under `cfg(test)`, skip the discovery fetch, done in
//! twenty lines. That branch *is* the algorithm-confusion vulnerability,
//! written down deliberately — and worse, it means the thing under test is not
//! the thing that ships. So [`oidc`] serves a real discovery document, a real
//! key set and real RS256 tokens over HTTP, and [`authenticator`] performs a
//! real WebAuthn ceremony with a real key pair. There is no branch anywhere in
//! `src/` that knows a test is running.

#![cfg(any(test, feature = "testing"))]

pub mod authenticator;
pub mod context;
pub mod oidc;

pub use authenticator::SoftAuthenticator;
pub use context::{TestServer, session_for};
pub use oidc::TestIdentityProvider;
