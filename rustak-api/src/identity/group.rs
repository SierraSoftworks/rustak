//! Channel names and the direction traffic flows in them.

use core::fmt;

use serde::{Deserialize, Serialize};

use super::newtype::string_newtype;

/// The longest channel name we will accept.
pub const MAX_LENGTH: usize = 128;

/// Characters a channel name may not contain.
///
/// A channel name is rendered into a `<groupList>` element of an ATAK
/// preference file, into the distinguished name TAK reports for the channel,
/// and into JSON; each of these would have to escape one of these characters,
/// and a name that survives the round trip in all three is worth more than a
/// name with punctuation in it.
pub const FORBIDDEN: &[char] = &['<', '>', '&', '"', '\'', '\\', ',', '=', '/', ';'];

/// The name of a channel (TAK calls them groups).
///
/// Case is preserved: TAK compares channel names exactly, and the default
/// channel is spelled [`GroupName::ANON`] in upper case.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupName(String);

impl GroupName {
    /// The channel every client is a member of unless configured otherwise.
    ///
    /// TAK gives this channel bit position 1 and treats it as the one an
    /// otherwise unconfigured client can still talk on.
    pub const ANON: &'static str = "__ANON__";

    /// The [`GroupName::ANON`] channel.
    pub fn anon() -> Self {
        Self(Self::ANON.to_string())
    }

    /// Validates a channel name.
    pub fn parse(raw: &str) -> Result<Self, GroupNameError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(GroupNameError::Empty);
        }

        let length = trimmed.chars().count();
        if length > MAX_LENGTH {
            return Err(GroupNameError::TooLong {
                length,
                max: MAX_LENGTH,
            });
        }

        if let Some(character) = trimmed
            .chars()
            .find(|c| c.is_control() || FORBIDDEN.contains(c))
        {
            return Err(GroupNameError::IllegalCharacter { character });
        }

        Ok(Self(trimmed.to_string()))
    }

    /// Wraps a name already known to be usable, such as one read back out of
    /// the database.
    pub fn from_storage(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Whether this is the default channel.
    pub fn is_anon(&self) -> bool {
        self.0 == Self::ANON
    }
}

string_newtype!(GroupName, GroupNameError, "a channel name");

/// The ways a channel name can be unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupNameError {
    /// The name was blank.
    Empty,

    /// The name was longer than [`MAX_LENGTH`].
    TooLong { length: usize, max: usize },

    /// The name held a character that would have to be escaped in the documents
    /// a channel name is written into.
    IllegalCharacter { character: char },
}

impl fmt::Display for GroupNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("A channel name cannot be blank."),
            Self::TooLong { length, max } => write!(
                f,
                "That channel name is {length} characters long, but the longest we can use is {max}."
            ),
            Self::IllegalCharacter { character } => write!(
                f,
                "A channel name cannot contain '{character}', because it would have to be escaped in the preference files and certificates the name is written into."
            ),
        }
    }
}

impl std::error::Error for GroupNameError {}

/// Which way traffic flows for a membership of a channel.
///
/// TAK's own vocabulary, which is the opposite way round from what most people
/// guess: `IN` is traffic coming *in* to the server, so it is permission to
/// write, and `OUT` is traffic going out to the client, so it is permission to
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Permission to send to the channel.
    #[serde(rename = "IN")]
    In,

    /// Permission to receive from the channel.
    #[serde(rename = "OUT")]
    Out,

    /// Permission to do both.
    ///
    /// Not a value TAK puts on the wire — its groups API only ever says `IN` or
    /// `OUT`, and storage keeps one row per direction. This exists so that
    /// granting someone full access to a channel is one choice in the UI rather
    /// than two, and [`Direction::expand`] turns it back into the pair.
    #[serde(rename = "BOTH")]
    Both,
}

impl Direction {
    /// Every direction, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::In, Self::Out, Self::Both];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::In => "IN",
            Self::Out => "OUT",
            Self::Both => "BOTH",
        }
    }

    /// A short phrase naming the direction for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::In => "Write",
            Self::Out => "Read",
            Self::Both => "Read and write",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_uppercase().as_str() {
            "IN" => Self::In,
            "OUT" => Self::Out,
            "BOTH" => Self::Both,
            _ => return None,
        })
    }

    /// The single directions this grant covers, which is what storage holds and
    /// what TAK's groups API reports.
    pub fn expand(&self) -> &'static [Self] {
        match self {
            Self::In => &[Self::In],
            Self::Out => &[Self::Out],
            Self::Both => &[Self::In, Self::Out],
        }
    }

    /// Whether this grant covers `other`.
    pub fn includes(&self, other: Self) -> bool {
        *self == Self::Both || *self == other
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_are_accepted_as_tak_spells_them() {
        for name in ["__ANON__", "Blue", "Team Alpha", "ops-1", "Ops.Command"] {
            assert!(
                GroupName::parse(name).is_ok(),
                "{name} should be a usable channel name"
            );
        }

        assert!(GroupName::anon().is_anon());
        assert!(!GroupName::parse("Blue").unwrap().is_anon());
    }

    #[test]
    fn channel_names_that_would_break_a_preference_file_are_refused() {
        for name in [
            "Blue<script>",
            "Blue&Red",
            "Blue\"Red",
            "Blue,Red",
            "Blue\nRed",
        ] {
            assert!(
                matches!(
                    GroupName::parse(name),
                    Err(GroupNameError::IllegalCharacter { .. })
                ),
                "{name} should be refused"
            );
        }

        assert_eq!(GroupName::parse("   "), Err(GroupNameError::Empty));
        assert!(matches!(
            GroupName::parse(&"a".repeat(MAX_LENGTH + 1)),
            Err(GroupNameError::TooLong { .. })
        ));
    }

    #[test]
    fn a_grant_of_both_directions_expands_to_the_pair_storage_holds() {
        assert_eq!(Direction::Both.expand(), &[Direction::In, Direction::Out]);
        assert_eq!(Direction::In.expand(), &[Direction::In]);

        assert!(Direction::Both.includes(Direction::In));
        assert!(Direction::Both.includes(Direction::Out));
        assert!(!Direction::In.includes(Direction::Out));
        assert!(Direction::In.includes(Direction::In));
    }

    #[test]
    fn directions_round_trip_through_their_tak_wire_form() {
        for direction in Direction::ALL.iter().copied() {
            let json = serde_json::to_string(&direction).unwrap();

            assert_eq!(json, format!("\"{}\"", direction.as_str()));
            assert_eq!(serde_json::from_str::<Direction>(&json).unwrap(), direction);
            assert_eq!(Direction::parse(direction.as_str()), Some(direction));
        }

        // TAK clients are not consistent about case in query parameters.
        assert_eq!(Direction::parse(" in "), Some(Direction::In));
        assert_eq!(Direction::parse("sideways"), None);
    }

    #[test]
    fn channel_names_round_trip_through_serde() {
        let name = GroupName::parse("Team Alpha").unwrap();
        let json = serde_json::to_string(&name).unwrap();

        assert_eq!(json, "\"Team Alpha\"");
        assert_eq!(serde_json::from_str::<GroupName>(&json).unwrap(), name);
        assert!(serde_json::from_str::<GroupName>("\"Blue&Red\"").is_err());
    }
}
