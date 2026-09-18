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
//! Only [`jwt`]: the access tokens rustak issues and the RSA keys behind them.
//! The principal, the Basic and bearer extractors, the rate limiter, the
//! filt-rs access control lists, the OAuth2 endpoints and the OIDC client
//! follow in M0-11 and M2, all of them resting on the token format pinned here.

pub mod jwt;

pub use jwt::{AccessClaims, JWT_HEADER_JSON, JwtIssuer};
