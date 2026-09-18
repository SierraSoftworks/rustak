//! The name a person or service signs in as.
//!
//! A username is the one identifier that travels through every layer of the
//! system: it is the `sub` of our JWT, the `CN` of the client certificate we
//! issue, the Basic-auth user an ATAK client enrols with, and the key an
//! administrator types into the UI. Each of those places has its own idea of
//! which characters are safe, so the rules here are the intersection of all of
//! them rather than anything one of them asks for on its own.
//!
//! Names are normalised to lower case. Identity providers are inconsistent
//! about the casing of `preferred_username`, CloudTAK lower-cases the username
//! before the password grant, and ATAK echoes back whatever the user typed — a
//! person whose records split across `Alice` and `alice` is a far worse outcome
//! than the theoretical case of two users differing only by case.

use core::fmt;

use super::newtype::string_newtype;

/// The longest username we will accept.
///
/// Certificate subjects, JWT claims and the `username=` parameter of an ATAK
/// enrolment URL all stay comfortable at this length.
pub const MAX_LENGTH: usize = 64;

/// Names owned by the installation, which no person may claim.
///
/// `anonymous` and `__anon__` are TAK's own well-known identities, and the
/// other two are how the server refers to itself in audit records.
pub const RESERVED: &[&str] = &["anonymous", "__anon__", "rustak", "takserver"];

/// The prefix reserved for identities the installation creates for itself.
pub const RESERVED_PREFIX: &str = "__";

/// Characters refused wherever they appear, ahead of the allow-list, so that
/// the refusal can say which rule the name broke.
///
/// `{` `}` `"` `\` and `,` would need escaping in the JWT claims and X.500
/// distinguished names a username is copied into; `=` `;` `/` separate the
/// fields of a distinguished name; `<` `>` would have to be escaped in the CoT
/// XML a callsign lookup renders.
pub const FORBIDDEN: &[char] = &['}', '{', '"', '\\', ',', '=', '/', ';', '<', '>'];

/// The name a person or service signs in as, normalised to lower case.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Username(String);

impl Username {
    /// Validates and normalises a name supplied by a person, an identity
    /// provider or a TAK client.
    pub fn parse(raw: &str) -> Result<Self, UsernameError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(UsernameError::Empty);
        }

        let normalised = trimmed.to_lowercase();

        let length = normalised.chars().count();
        if length > MAX_LENGTH {
            return Err(UsernameError::TooLong {
                length,
                max: MAX_LENGTH,
            });
        }

        if RESERVED.contains(&normalised.as_str()) || normalised.starts_with(RESERVED_PREFIX) {
            return Err(UsernameError::Reserved {
                username: normalised,
            });
        }

        if let Some(character) = normalised.chars().find(|c| FORBIDDEN.contains(c)) {
            return Err(UsernameError::ForbiddenCharacter { character });
        }

        if let Some(character) = normalised.chars().find(|c| !is_allowed(*c)) {
            return Err(UsernameError::IllegalCharacter { character });
        }

        // A leading punctuation mark is what makes a name confusable with a
        // flag, a relative path or a reserved identity, so the first character
        // has to carry meaning of its own.
        let first = normalised.chars().next().unwrap_or_default();
        if !first.is_ascii_alphanumeric() {
            return Err(UsernameError::MustStartAlphanumeric { character: first });
        }

        Ok(Self(normalised))
    }

    /// Wraps a name already known to be usable, such as one read back out of
    /// the database.
    ///
    /// Normalises case exactly as [`Username::parse`] does but performs no
    /// validation, so a row written by an older version under rules we have
    /// since tightened stays readable rather than becoming unloadable.
    pub fn from_storage(value: impl AsRef<str>) -> Self {
        Self(value.as_ref().to_lowercase())
    }

    /// Whether this name matches a certificate's common name.
    ///
    /// Enrolment compares the `CN` of a submitted CSR against the authenticated
    /// user, and clients are not consistent about the case they put there.
    pub fn eq_ignore_case(&self, cn: &str) -> bool {
        self.0.eq_ignore_ascii_case(cn.trim())
    }
}

/// Whether a character may appear in a normalised username.
///
/// The set is deliberately small: lower-case ASCII alphanumerics plus the
/// punctuation that real directories put in a name — `.` and `_` for
/// `first.last`, `@` for an email-shaped name, `+` for a tagged address, and
/// `-` for a hyphenated one.
fn is_allowed(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '@' | '+' | '-')
}

string_newtype!(Username, UsernameError, "a username");

/// The ways a name can fail to be usable as a username.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsernameError {
    /// The name was blank, or was nothing but whitespace.
    Empty,

    /// The name was longer than [`MAX_LENGTH`].
    TooLong { length: usize, max: usize },

    /// The name is one the installation keeps for itself.
    Reserved { username: String },

    /// The name contained a character that cannot be carried safely through a
    /// certificate subject, a JWT claim or CoT XML.
    ForbiddenCharacter { character: char },

    /// The name contained a character outside the permitted set.
    IllegalCharacter { character: char },

    /// The name began with punctuation rather than a letter or digit.
    MustStartAlphanumeric { character: char },
}

impl fmt::Display for UsernameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("A username cannot be blank."),
            Self::TooLong { length, max } => write!(
                f,
                "That username is {length} characters long, but the longest we can use is {max}."
            ),
            Self::Reserved { username } => write!(
                f,
                "The username '{username}' is reserved by the server and cannot be signed in as."
            ),
            Self::ForbiddenCharacter { character } => write!(
                f,
                "A username cannot contain '{character}', because it would have to be escaped in the certificates and tokens the name is copied into."
            ),
            Self::IllegalCharacter { character } => write!(
                f,
                "A username cannot contain '{character}'. Use letters, digits, and any of . _ @ + -"
            ),
            Self::MustStartAlphanumeric { character } => write!(
                f,
                "A username has to start with a letter or a digit, not '{character}'."
            ),
        }
    }
}

impl std::error::Error for UsernameError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_accepted() {
        for name in [
            "alice",
            "alice.smith",
            "alice_smith",
            "alice-smith",
            "alice@example.com",
            "alice+atak@example.com",
            "0d4f1a6e1d2b4c3a9f8e7a6b5c4d3e2f",
            "a",
        ] {
            assert!(
                Username::parse(name).is_ok(),
                "{name} should be a usable username: {:?}",
                Username::parse(name)
            );
        }
    }

    #[test]
    fn names_are_lower_cased_and_trimmed() {
        assert_eq!(Username::parse("  Alice  ").unwrap().as_str(), "alice");
        assert_eq!(
            Username::parse("ALICE@Example.COM").unwrap(),
            Username::parse("alice@example.com").unwrap()
        );
    }

    #[test]
    fn a_certificate_common_name_matches_regardless_of_case() {
        let username = Username::parse("alice").unwrap();

        assert!(username.eq_ignore_case("Alice"));
        assert!(username.eq_ignore_case(" ALICE "));
        assert!(!username.eq_ignore_case("bob"));
    }

    #[test]
    fn blank_and_overlong_names_are_refused() {
        assert_eq!(Username::parse(""), Err(UsernameError::Empty));
        assert_eq!(Username::parse("   "), Err(UsernameError::Empty));

        assert!(Username::parse(&"a".repeat(MAX_LENGTH)).is_ok());
        assert!(matches!(
            Username::parse(&"a".repeat(MAX_LENGTH + 1)),
            Err(UsernameError::TooLong { .. })
        ));
    }

    #[test]
    fn names_the_installation_owns_cannot_be_claimed() {
        // Otherwise a person could sign in as the identity TAK gives to
        // unauthenticated traffic, or as the server itself in the audit log.
        for name in ["anonymous", "ANONYMOUS", "__anon__", "rustak", "takserver"] {
            assert!(
                matches!(Username::parse(name), Err(UsernameError::Reserved { .. })),
                "{name} should be reserved"
            );
        }

        assert!(matches!(
            Username::parse("__internal"),
            Err(UsernameError::Reserved { .. })
        ));
    }

    #[test]
    fn characters_that_would_need_escaping_downstream_are_refused() {
        // Each of these ends up inside an X.500 distinguished name, a JWT
        // claim or CoT XML, where it would change the meaning of the document
        // rather than appear literally.
        for name in [
            "alice,bob",
            "alice=bob",
            "alice/bob",
            "alice;bob",
            "alice<bob",
            "alice>bob",
            "alice\"bob",
            "alice\\bob",
            "alice{bob",
            "alice}bob",
        ] {
            assert!(
                matches!(
                    Username::parse(name),
                    Err(UsernameError::ForbiddenCharacter { .. })
                ),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn characters_outside_the_permitted_set_are_refused() {
        for name in [
            "alice bob",
            "alice\nbob",
            "alice!",
            "ali\u{00e7}e",
            "alice#1",
        ] {
            assert!(
                matches!(
                    Username::parse(name),
                    Err(UsernameError::IllegalCharacter { .. })
                ),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn a_name_has_to_start_with_a_letter_or_a_digit() {
        for name in [".alice", "-alice", "@alice", "+alice"] {
            assert!(
                matches!(
                    Username::parse(name),
                    Err(UsernameError::MustStartAlphanumeric { .. })
                ),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn usernames_round_trip_through_serde() {
        let username = Username::parse("alice@example.com").unwrap();
        let json = serde_json::to_string(&username).unwrap();

        assert_eq!(json, "\"alice@example.com\"");
        assert_eq!(serde_json::from_str::<Username>(&json).unwrap(), username);
    }

    #[test]
    fn a_malformed_name_is_refused_at_the_serde_boundary() {
        // The wire is untrusted, so a request body carrying an unusable name
        // fails to deserialise rather than reaching a handler.
        assert!(serde_json::from_str::<Username>("\"alice bob\"").is_err());
        assert!(serde_json::from_str::<Username>("\"anonymous\"").is_err());
    }

    #[test]
    fn stored_values_load_even_if_they_would_fail_todays_validation() {
        let stored = Username::from_storage("Alice Legacy");

        assert_eq!(stored.as_str(), "alice legacy");
    }

    #[test]
    fn debug_shows_the_name() {
        let username = Username::parse("alice").unwrap();

        assert_eq!(format!("{username:?}"), "Username(alice)");
        assert_eq!(format!("{username}"), "alice");
    }
}
