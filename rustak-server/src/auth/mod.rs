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
//! # What M2 added
//!
//! The client-certificate arm ([`cert`]) and the Basic arm ([`basic`]), and the
//! [`ListenerAuthPolicy`] that says which of the three a listener accepts.
//! [`resolve::resolve_principal`] is the one function a listener calls; the
//! `/login/*` federation is still to come.

pub mod acl;
pub mod basic;
pub mod cert;
pub mod jwt;
pub mod mission_token;
pub mod passkey_store;
pub mod passkeys;
pub mod ratelimit;
pub mod resolve;
pub mod setup;
pub mod tokens;

pub use acl::{AclOutcome, AuthRequestFilter};
pub use basic::{BasicCredential, basic_credential};
pub use cert::client_cert;
pub use jwt::{AccessClaims, JWT_HEADER_JSON, JwtIssuer};
pub use mission_token::{MissionClaims, MissionTokens, TokenError, TokenType, mission_bearer};
pub use passkeys::Passkeys;
pub use ratelimit::RateLimiter;
pub use resolve::{AuthFailure, BasicPolicy, ListenerAuthPolicy, Resolved, resolve_principal};
pub use setup::SetupToken;
