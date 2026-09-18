//! Proof key for code exchange.
//!
//! The verifier is a high-entropy random string the client keeps; the challenge
//! is its SHA-256, base64url without padding, which is all the provider ever
//! sees until the code is redeemed. An intercepted code is then worthless
//! without the verifier.
//!
//! Only `S256` is produced. The `plain` method the specification also permits
//! sends the verifier itself in the authorization request, which defeats the
//! entire mechanism.

use base64::Engine as _;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

/// How many random bytes a verifier carries.
///
/// RFC 7636 allows 43 to 128 characters; 32 bytes encode to 43, which is the
/// shortest the specification permits and already 256 bits of entropy.
const VERIFIER_BYTES: usize = 32;

/// Base64 as OAuth uses it here: URL-safe, unpadded.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A verifier and the challenge derived from it.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    /// Kept by whoever started the flow, sent only when the code is redeemed.
    pub verifier: String,
    /// Sent to the provider in the authorization request.
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    /// Written out because the verifier is the secret half: a `{:?}` in a log
    /// line would hand somebody watching the redirect everything they need.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"***")
            .field("challenge", &self.challenge)
            .finish()
    }
}

/// Mints a fresh pair.
pub fn pkce_pair() -> Pkce {
    let mut bytes = [0u8; VERIFIER_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    let verifier = B64.encode(bytes);
    let challenge = challenge_for(&verifier);

    Pkce {
        verifier,
        challenge,
    }
}

/// The `S256` challenge for a verifier.
pub fn challenge_for(verifier: &str) -> String {
    B64.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_sha256_of_the_verifier() {
        let pkce = pkce_pair();

        assert_eq!(pkce.challenge, challenge_for(&pkce.verifier));
        assert_ne!(pkce.challenge, pkce.verifier, "S256, never plain");
    }

    #[test]
    fn a_verifier_is_within_the_length_the_specification_allows() {
        let pkce = pkce_pair();

        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "the unreserved character set, so it survives a query string intact",
        );
    }

    #[test]
    fn two_flows_never_share_a_verifier() {
        assert_ne!(pkce_pair().verifier, pkce_pair().verifier);
    }

    #[test]
    fn a_known_vector_matches_the_specifications_example() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_verifier_is_redacted_when_a_pair_is_printed() {
        let pkce = pkce_pair();
        let rendered = format!("{pkce:?}");

        assert!(rendered.contains("***"));
        assert!(
            !rendered.contains(&pkce.verifier),
            "the secret half must not reach a log line",
        );
        assert!(rendered.contains(&pkce.challenge));
    }
}
