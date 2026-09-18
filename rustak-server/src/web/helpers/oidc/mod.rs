//! The OpenID Connect client: discovery, verification, and the confidential
//! code exchange.
//!
//! # How a sign-in works here
//!
//! The browser is a **public** client and runs the authorization request
//! itself; rustak is the **confidential** one and holds the client secret. The
//! browser sends us the authorization code, we exchange it, verify the ID token
//! the provider returns, decide who that is, and then issue **our own** token.
//! The provider's ID token never becomes a credential for this API — a
//! deliberate difference from automate, where it was the bearer — because
//! rustak is the identity authority for everything else that connects to it and
//! one token format for all of them is what makes that possible.
//!
//! # What the browser has to prove
//!
//! Proof key for code exchange is required even though the client secret
//! already binds the exchange, because the code travels through a redirect the
//! browser controls: without a verifier, a code intercepted from the address
//! bar or a logged referrer could be redeemed by whoever found it. The nonce
//! does the matching job for the ID token, binding it to the one flow that
//! asked for it.
//!
//! Neither is something we can check *for* the browser — we can only refuse to
//! proceed without them — so [`exchange::exchange_code`] passes the verifier
//! through and [`validate::validate_token`] insists on the nonce when one was
//! issued.

pub mod claims;
pub mod discovery;
pub mod exchange;
pub mod pkce;
pub mod validate;

pub use claims::{filterable_claims, groups_from_claims, identity_from_claims};
pub use discovery::{OidcDiscovery, discovery};
pub use exchange::{TokenSet, exchange_code, refresh_tokens};
pub use pkce::{Pkce, pkce_pair};
pub use validate::validate_token;
