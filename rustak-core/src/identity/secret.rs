//! Secrets in memory: a string that does not print itself and does not linger.
//!
//! Every credential rustak mints — an enrolment token, a client password, a
//! service token — exists in plaintext exactly once, on the way to the operator
//! who asked for it. [`Secret`] is the type it travels in. It refuses to appear
//! in a log line, a `Debug` dump, a panic message or a serialised structure, and
//! it wipes its buffer when it is dropped.
//!
//! That last part is a mitigation, not a guarantee: a `String` that reallocates
//! while it grows leaves its old buffer behind, so a [`Secret`] is built once
//! from a finished value rather than pushed into. What it does reliably prevent
//! is the plaintext outliving the request in a freed-but-unzeroed heap block
//! that a later core dump or heap dump would include.
//!
//! # Two shapes of generated secret
//!
//! [`generate_token`] produces URL-safe base64 for anything a machine reads: an
//! enrolment token in a `tak://` QR code, a service token in a config file.
//!
//! [`generate_password`] produces Crockford base32 in hyphenated groups —
//! `xk4m-9r2t-h7wc-5nqb-e3fd` — for the one credential a human has to retype:
//! the opt-in client password that CloudTAK's password grant needs. Crockford's
//! alphabet has no `I`, `L`, `O` or `U`, so there is no pair of characters that
//! can be confused when the value is read off a screen and typed into a phone,
//! and no accidental word to be embarrassed by.

use std::fmt;

use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine as _};
use rand::Rng as _;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Crockford base32, which drops `I`, `L`, `O` and `U` so that nothing in it can
/// be misread when a person copies it off a screen.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Characters per hyphen-separated group in a generated password.
const PASSWORD_GROUP: usize = 4;

/// Bytes of entropy behind a generated token, giving 43 base64 characters.
pub const DEFAULT_TOKEN_BYTES: usize = 32;

/// Groups in a generated password: 20 Crockford characters, 100 bits.
pub const DEFAULT_PASSWORD_GROUPS: usize = 5;

/// A plaintext secret that will not print itself and wipes itself when dropped.
///
/// ```
/// # use rustak_core::identity::Secret;
/// let secret = Secret::new("s3cr3t");
///
/// // The one way to read it is to say so.
/// assert_eq!(secret.expose(), "s3cr3t");
///
/// // Every other way says nothing, including the one a `tracing` field or a
/// // `dbg!` would reach for.
/// assert_eq!(format!("{secret:?}"), "Secret(***)");
/// ```
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value that is already in hand.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reads the plaintext.
    ///
    /// Named so that every place a secret leaves this type is greppable, and so
    /// that doing it by accident is not possible.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The length of the secret, which is not itself a secret.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the secret is empty — an unset credential rather than a bad one.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Compares against a candidate in time that does not depend on how much of
    /// the value matched.
    ///
    /// For anything that is *stored*, verify the argon2id hash instead
    /// ([`super::password::verify`]); this is for the values we hold in memory
    /// and compare directly, such as the first-run setup token.
    ///
    /// The comparison leaks the length of the two values, which is not
    /// information an attacker can use against a fixed-length generated secret.
    pub fn constant_time_eq(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let candidate = candidate.as_bytes();

        // Every byte of the candidate is compared, with no early return on the
        // first mismatch, so the time taken depends only on how long the
        // candidate is. The length comparison is folded in at the end rather
        // than short-circuiting at the start, for the same reason.
        let mut difference = 0u8;
        for (index, byte) in candidate.iter().enumerate() {
            difference |= byte ^ expected.get(index).copied().unwrap_or(0);
        }

        difference == 0 && expected.len() == candidate.len()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    /// Redacted, deliberately: `{}` in a log line or an error message must not
    /// be the one place a secret escapes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("***")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Mints a URL-safe token with `bytes` of entropy behind it.
///
/// This is what goes in an enrolment QR code and a service token: read by
/// machines, never retyped, so density beats legibility.
pub fn generate_token(bytes: usize) -> Secret {
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);

    let token = BASE64_URL_SAFE_NO_PAD.encode(&buffer);
    buffer.zeroize();

    Secret(token)
}

/// Mints a password a person can read off a screen and type into a phone.
///
/// `groups` groups of four Crockford base32 characters, hyphen-separated — five
/// groups is 100 bits. See the [module documentation](self) for why this
/// alphabet.
pub fn generate_password(groups: usize) -> Secret {
    let mut buffer = vec![0u8; groups * PASSWORD_GROUP];
    rand::rng().fill_bytes(&mut buffer);

    let mut password = String::with_capacity(groups * (PASSWORD_GROUP + 1));
    for (index, byte) in buffer.iter().enumerate() {
        if index > 0 && index % PASSWORD_GROUP == 0 {
            password.push('-');
        }

        // 32 divides 256 exactly, so masking is uniform — no modulo bias, and
        // no rejection loop whose timing would depend on the bytes drawn.
        password.push(CROCKFORD[(byte & 0x1F) as usize] as char);
    }
    buffer.zeroize();

    Secret(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_says_nothing_in_any_of_the_ways_it_might_be_printed() {
        // `{:?}` is what a `tracing` field, a `dbg!` and a failed `assert_eq!`
        // all reach for, and `{}` is what an error message does. Both have to be
        // dead ends or the type is decoration.
        let secret = Secret::new("hunter2");

        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(format!("{secret}"), "***");
        assert!(!format!("{secret:?} {secret}").contains("hunter2"));
    }

    #[test]
    fn the_only_way_to_read_a_secret_is_to_say_so() {
        assert_eq!(Secret::new("hunter2").expose(), "hunter2");
    }

    #[test]
    fn a_generated_token_is_url_safe_so_it_survives_a_qr_code_and_a_query_string() {
        // Enrolment tokens travel inside `tak://com.atakmap.app/enroll?token=`,
        // so a `+` or a `/` would have to be escaped by every reader of it.
        let token = generate_token(DEFAULT_TOKEN_BYTES);

        assert!(
            token
                .expose()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{token:?} should be URL-safe",
        );
        assert_eq!(token.len(), 43, "32 bytes is 43 unpadded base64 characters");
    }

    #[test]
    fn a_generated_password_has_no_character_a_person_could_misread() {
        // The whole reason for Crockford's alphabet: this value is read off a
        // screen and typed into a phone, where `I`/`1` and `O`/`0` are the two
        // most common transcription failures.
        let password = generate_password(DEFAULT_PASSWORD_GROUPS);

        for forbidden in ['I', 'L', 'O', 'U'] {
            assert!(
                !password.expose().contains(forbidden),
                "{password:?} should not contain {forbidden}",
            );
        }
    }

    #[test]
    fn a_generated_password_is_grouped_so_it_can_be_read_aloud() {
        let password = generate_password(DEFAULT_PASSWORD_GROUPS);
        let groups: Vec<&str> = password.expose().split('-').collect();

        assert_eq!(groups.len(), DEFAULT_PASSWORD_GROUPS);
        assert!(groups.iter().all(|group| group.len() == PASSWORD_GROUP));
    }

    #[test]
    fn two_generated_secrets_are_never_the_same() {
        // Stated as the property rather than assumed from the use of an RNG,
        // because a seeding mistake here mints the same enrolment token twice.
        let mut seen = std::collections::HashSet::new();

        for _ in 0..64 {
            assert!(seen.insert(generate_token(DEFAULT_TOKEN_BYTES).expose().to_string()));
            assert!(
                seen.insert(
                    generate_password(DEFAULT_PASSWORD_GROUPS)
                        .expose()
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn a_constant_time_comparison_still_gives_the_right_answer() {
        // Constant-time code that is merely constant would be easy to write and
        // useless, so pin the behaviour as well as the intent.
        let secret = Secret::new("setup-token-value");

        assert!(secret.constant_time_eq("setup-token-value"));
        assert!(!secret.constant_time_eq("setup-token-valuf"));
        assert!(!secret.constant_time_eq("setup-token-valu"));
        assert!(!secret.constant_time_eq("setup-token-value-and-more"));
        assert!(!secret.constant_time_eq(""));
    }

    #[test]
    fn an_empty_secret_compares_as_unset_rather_than_matching_everything() {
        let empty = Secret::new("");

        assert!(empty.is_empty());
        assert!(empty.constant_time_eq(""));
        assert!(!empty.constant_time_eq("anything"));
    }
}
