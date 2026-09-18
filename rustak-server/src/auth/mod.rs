//! Who a request speaks for, and what it is allowed to do.
//!
//! rustak is the identity authority for everything that connects to it. A
//! device presents a certificate this server's CA issued; a browser or a
//! sidecar presents a token this server signed. Either way the answer to "who
//! is this?" comes from here rather than from an upstream system — even when an
//! identity provider did the signing in, because the provider tells us who the
//! person is and we decide what a session with us then looks like.
//!
//! # What is here in M0
//!
//! The token format ([`jwt`]) and the sessions built on it ([`tokens`]); the
//! passkey ceremonies that are the only way in without an identity provider
//! ([`passkeys`]); the one-time tokens the first run depends on ([`setup`]);
//! the access-control expressions ([`acl`]); the rate limiter every credential
//! endpoint goes through ([`ratelimit`]); and the bearer resolution the admin
//! API sits behind ([`resolve`]).
//!
//! The client-certificate and Basic paths, the OAuth2 endpoints and the
//! `/login/*` federation arrive with enrolment in M2, all of them beside
//! [`resolve`] and all resting on the token format pinned here.

pub mod acl;
pub mod jwt;
pub mod passkey_store;
pub mod passkeys;
pub mod ratelimit;
pub mod resolve;
pub mod setup;
pub mod tokens;

pub use acl::{AclOutcome, AuthRequestFilter};
pub use jwt::{AccessClaims, JWT_HEADER_JSON, JwtIssuer};
pub use passkeys::Passkeys;
pub use ratelimit::RateLimiter;
pub use resolve::{AuthFailure, Resolved};
pub use setup::SetupToken;
