//! rustak as an OAuth2 authorization server, and as an OpenID Connect relying
//! party — the two halves of the same browser sign-in.
//!
//! # The shape of the flow
//!
//! A TAK-style browser client sends somebody to [`mod@authorize`]'s
//! `GET /oauth/authorize`. If there is already a session on the request we mint
//! an authorization code and send them straight back. If there is not, we start
//! an OpenID Connect flow of our own against the configured identity provider —
//! [`login`]'s `GET /login/auth` is the same thing with no client behind it —
//! and the provider returns the browser to `GET /login/redirect`, where the
//! session is established and the code (if a client was waiting for one) is
//! finally issued.
//!
//! So there are **two** authorization codes in flight and they are not the same
//! thing: the provider's, which we redeem with the client secret and a proof
//! key of our own ([`state`] holds that verifier), and ours, which the client
//! redeems at `POST /oauth/token` with the proof key *it* generated
//! ([`codes`] holds that challenge). Confusing the two is the mistake this
//! module tree is laid out to make hard.
//!
//! # What each control is for
//!
//! | Control | Stops |
//! |---|---|
//! | `state` cookie, compared as `sha256(cookie) == state` | a callback replayed into somebody else's browser |
//! | one-shot [`state::PendingAuth`], ten minutes | that same callback replayed into *this* browser twice |
//! | our proof key on the provider's code | a code lifted from a redirect or a proxy log being redeemed |
//! | our `nonce` in the provider's ID token | an ID token minted for a different flow being presented to this one |
//! | the client's proof key on our code ([`codes`]) | a code lifted from a **public** client's redirect being redeemed |
//! | the client's secret at `/oauth/token` ([`clients`]) | the same, for a **confidential** client, which is what lets its proof key be optional |
//! | exact `redirect_uri` match, at issue and at redemption | a code being delivered to, or redeemed against, another registered URI |
//! | exact `post_logout_redirect_uri` match ([`session`]) | `/logout` becoming an open redirector on the path every session ends at |
//! | the client's `nonce` echoed into **our** ID token ([`id_token`]) | one of our ID tokens being replayed into a relying party's other flow |
//! | `aud` = the one client id | one of our ID tokens being presented to a different relying party |
//!
//! None of them is redundant. The state cookie is a cross-site-request-forgery
//! control and knows nothing about codes; proof key for code exchange is a code
//! theft control and knows nothing about which browser is asking; the nonce
//! binds the *token* rather than the *code*. Removing any one of them leaves a
//! flow that still works in a browser and no longer resists the attack that one
//! was there for.
//!
//! The one row that *replaces* another is client authentication. A public
//! client has no secret, so its proof key is the only thing between an
//! intercepted code and a session and stays mandatory. A confidential client
//! presents a secret an interceptor does not have, so RFC 6749 lets the proof
//! key be optional there — and it has to be, because CloudTAK's relying party
//! sends no `code_challenge` at all. `--check` refuses a client that is
//! configured as one and credentialled as the other, so neither can be half a
//! control.
//!
//! # rustak as an OpenID provider (M8-01)
//!
//! [`discovery`] publishes what a relying party needs to find, [`jwt::jwks`]
//! the keys it verifies with, [`id_token`] the assertion it reads and
//! [`mod@userinfo`] the account behind it. The access token that comes out of
//! same exchange is an ordinary rustak token, which is the point: the client
//! holding it can go straight on to `/Marti/api/tls/*` and enrol.
//!
//! [`jwt::jwks`]: crate::auth::JwtIssuer::jwks
//!
//! # Cookies
//!
//! [`cookies`] holds the `access_token_N` chunking TAK Server invented and the
//! rule that says where a cookie may be used as a credential at all: the
//! `/login/*` endpoints and the Marti surface, never `/api/v1`. That admin API
//! is bearer-only by construction — its middleware reads the `Authorization`
//! header and nothing else — so it has no cross-site-request-forgery surface,
//! and this module tree must not give it one.

pub mod authorize;
pub mod claims;
pub mod clients;
pub mod code_grant;
pub mod codes;
pub mod cookies;
pub mod discovery;
pub mod id_token;
pub mod login;
pub mod responses;
pub mod scopes;
pub mod session;
pub mod state;
pub mod userinfo;

pub use authorize::authorize;
pub use codes::{CodeError, NewCode, Redemption};
pub use cookies::{access_token_from_cookies, cookies_allowed};
pub use discovery::openid_configuration;
pub use state::{PendingAuth, PendingKind};
pub use userinfo::userinfo;

/// Compares two strings without leaking where they first differ.
///
/// Used for the `state` comparison and for the proof-key challenge. Both are
/// digests of high-entropy values, so a timing signal would be hard to use —
/// but "hard to use" is not a property worth relying on when the alternative is
/// four lines.
pub(crate) fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |differences, (left, right)| {
            differences | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_equal_strings_compare_equal_and_nothing_else_does() {
        assert!(constant_time_eq("abcd", "abcd"));
        assert!(!constant_time_eq("abcd", "abce"));
        assert!(!constant_time_eq("abcd", "abc"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
