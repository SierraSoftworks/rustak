//! The identifiers clients and services announce themselves by.

use core::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::newtype::string_newtype;

/// The longest device identifier we will accept.
///
/// TAK clients mint these themselves — an Android serial, a UUID, or a label a
/// bridge made up — so the bound exists to keep one out of the index rather
/// than to describe anything real.
pub const MAX_UID_LENGTH: usize = 256;

/// The shortest and longest a service name may be.
pub const SERVICE_NAME_LENGTH: std::ops::RangeInclusive<usize> = 2..=63;

/// The prefix a sidecar's device identifier is built from.
pub const SERVICE_UID_PREFIX: &str = "SERVICE-";

/// The identifier a client uses for itself: ATAK's `clientUid`.
///
/// Case is preserved, because a device identifier is opaque to us — it is
/// whatever the client sent, and it has to match byte for byte when the same
/// client comes back.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceUid(String);

impl DeviceUid {
    /// Validates an identifier a client supplied.
    ///
    /// The rules are deliberately loose. Interior spaces and brackets are
    /// allowed because CloudTAK enrols with `clientUid=<username> (ETL)`, so
    /// refusing them would refuse CloudTAK. What is refused is what would let
    /// an identifier forge a line in a log or an attribute in a document:
    /// control characters, including the newline and the NUL byte.
    pub fn parse(raw: &str) -> Result<Self, DeviceUidError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DeviceUidError::Empty);
        }

        let length = trimmed.chars().count();
        if length > MAX_UID_LENGTH {
            return Err(DeviceUidError::TooLong {
                length,
                max: MAX_UID_LENGTH,
            });
        }

        if let Some(character) = trimmed.chars().find(|c| c.is_control()) {
            return Err(DeviceUidError::ControlCharacter {
                code: character as u32,
            });
        }

        Ok(Self(trimmed.to_string()))
    }

    /// Wraps an identifier already known to be usable, such as one read back
    /// out of the database.
    pub fn from_storage(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The service this identifier belongs to, if it is a sidecar's.
    pub fn service_name(&self) -> Option<ServiceName> {
        self.0
            .strip_prefix(SERVICE_UID_PREFIX)
            .and_then(|name| ServiceName::parse(name).ok())
    }
}

string_newtype!(DeviceUid, DeviceUidError, "a device identifier");

/// The ways a client identifier can be unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceUidError {
    /// The identifier was blank.
    Empty,

    /// The identifier was longer than [`MAX_UID_LENGTH`].
    TooLong { length: usize, max: usize },

    /// The identifier carried a control character, which could break out of a
    /// log line or an XML attribute.
    ControlCharacter { code: u32 },
}

impl fmt::Display for DeviceUidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("A client did not supply a device identifier."),
            Self::TooLong { length, max } => write!(
                f,
                "That device identifier is {length} characters long, but the longest we accept is {max}."
            ),
            Self::ControlCharacter { code } => write!(
                f,
                "A device identifier cannot contain the control character U+{code:04X}."
            ),
        }
    }
}

impl std::error::Error for DeviceUidError {}

/// The name of a sidecar service.
///
/// Service names appear in URLs, in configuration keys and in the device
/// identifier a sidecar connects with, so they are restricted to the shape that
/// is unambiguous in all three: lower-case, hyphen-separated, starting with a
/// letter or digit.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceName(String);

impl ServiceName {
    /// Validates and normalises a service name.
    pub fn parse(raw: &str) -> Result<Self, ServiceNameError> {
        let normalised = raw.trim().to_lowercase();

        let length = normalised.chars().count();
        if !SERVICE_NAME_LENGTH.contains(&length) {
            return Err(ServiceNameError::WrongLength {
                length,
                min: *SERVICE_NAME_LENGTH.start(),
                max: *SERVICE_NAME_LENGTH.end(),
            });
        }

        if let Some(character) = normalised
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
        {
            return Err(ServiceNameError::IllegalCharacter { character });
        }

        let first = normalised.chars().next().unwrap_or_default();
        if !first.is_ascii_alphanumeric() {
            return Err(ServiceNameError::MustStartAlphanumeric { character: first });
        }

        Ok(Self(normalised))
    }

    /// Wraps a name already known to be usable.
    pub fn from_storage(value: impl AsRef<str>) -> Self {
        Self(value.as_ref().to_lowercase())
    }

    /// The device identifier this service connects to the stream with.
    ///
    /// A sidecar is a client like any other, so it needs a `clientUid`; deriving
    /// it from the name means the same service always reappears as the same
    /// device rather than accumulating one row per restart.
    pub fn uid(&self) -> DeviceUid {
        DeviceUid(format!("{SERVICE_UID_PREFIX}{}", self.0))
    }
}

string_newtype!(ServiceName, ServiceNameError, "a service name");

/// The ways a service name can be unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceNameError {
    /// The name was outside [`SERVICE_NAME_LENGTH`].
    WrongLength {
        length: usize,
        min: usize,
        max: usize,
    },

    /// The name contained something other than a lower-case letter, a digit or
    /// a hyphen.
    IllegalCharacter { character: char },

    /// The name began with a hyphen.
    MustStartAlphanumeric { character: char },
}

impl fmt::Display for ServiceNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { length, min, max } => write!(
                f,
                "A service name has to be between {min} and {max} characters long, and that one is {length}."
            ),
            Self::IllegalCharacter { character } => write!(
                f,
                "A service name cannot contain '{character}'. Use lower-case letters, digits and hyphens."
            ),
            Self::MustStartAlphanumeric { character } => write!(
                f,
                "A service name has to start with a letter or a digit, not '{character}'."
            ),
        }
    }
}

impl std::error::Error for ServiceNameError {}

/// The globally unique identifier of a mission.
///
/// TAK generates these client-side and uses them as the stable key for a
/// mission whose name has changed, so we store what we were given rather than
/// minting our own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MissionGuid(Uuid);

impl MissionGuid {
    /// Parses the hyphenated form TAK puts on the wire.
    pub fn parse(raw: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(raw.trim())?))
    }

    /// Wraps an identifier we already hold.
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    /// The underlying identifier.
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for MissionGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for MissionGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MissionGuid({})", self.0)
    }
}

impl From<Uuid> for MissionGuid {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl core::str::FromStr for MissionGuid {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identifiers_real_clients_send_are_accepted() {
        for uid in [
            "ANDROID-358240051111110",
            "0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f",
            // CloudTAK enrols with the username followed by " (ETL)", so a
            // device identifier has to survive spaces and brackets.
            "alice (ETL)",
            "S-1-5-21-WINTAK",
        ] {
            assert!(
                DeviceUid::parse(uid).is_ok(),
                "{uid} should be a usable device identifier"
            );
        }

        assert_eq!(
            DeviceUid::parse("  alice (ETL) ").unwrap().as_str(),
            "alice (ETL)"
        );
    }

    #[test]
    fn unusable_device_identifiers_are_refused() {
        assert_eq!(DeviceUid::parse("  "), Err(DeviceUidError::Empty));

        assert!(matches!(
            DeviceUid::parse(&"a".repeat(MAX_UID_LENGTH + 1)),
            Err(DeviceUidError::TooLong { .. })
        ));

        // A newline would let an identifier forge a second line in a log, and a
        // NUL would truncate it in anything reading C strings.
        for uid in ["ANDROID\n-1", "ANDROID\u{0}1"] {
            assert!(matches!(
                DeviceUid::parse(uid),
                Err(DeviceUidError::ControlCharacter { .. })
            ));
        }
    }

    #[test]
    fn a_service_connects_as_a_device_named_after_itself() {
        let name = ServiceName::parse("weather-feed").unwrap();
        let uid = name.uid();

        assert_eq!(uid.as_str(), "SERVICE-weather-feed");
        assert_eq!(uid.service_name(), Some(name));
        assert_eq!(DeviceUid::parse("ANDROID-1").unwrap().service_name(), None);
    }

    #[test]
    fn service_names_are_normalised_and_bounded() {
        assert_eq!(ServiceName::parse(" Weather ").unwrap().as_str(), "weather");

        assert!(matches!(
            ServiceName::parse("a"),
            Err(ServiceNameError::WrongLength { .. })
        ));
        assert!(matches!(
            ServiceName::parse(&"a".repeat(64)),
            Err(ServiceNameError::WrongLength { .. })
        ));
        assert!(matches!(
            ServiceName::parse("weather_feed"),
            Err(ServiceNameError::IllegalCharacter { character: '_' })
        ));
        assert!(matches!(
            ServiceName::parse("-weather"),
            Err(ServiceNameError::MustStartAlphanumeric { .. })
        ));
    }

    #[test]
    fn identifiers_round_trip_through_serde() {
        let uid = DeviceUid::parse("alice (ETL)").unwrap();
        let json = serde_json::to_string(&uid).unwrap();
        assert_eq!(json, "\"alice (ETL)\"");
        assert_eq!(serde_json::from_str::<DeviceUid>(&json).unwrap(), uid);

        let name = ServiceName::parse("weather-feed").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"weather-feed\"");
        assert_eq!(serde_json::from_str::<ServiceName>(&json).unwrap(), name);

        let guid = MissionGuid::parse("0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f").unwrap();
        let json = serde_json::to_string(&guid).unwrap();
        assert_eq!(json, "\"0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f\"");
        assert_eq!(serde_json::from_str::<MissionGuid>(&json).unwrap(), guid);
    }

    #[test]
    fn a_malformed_identifier_is_refused_at_the_serde_boundary() {
        assert!(serde_json::from_str::<DeviceUid>("\"\"").is_err());
        assert!(serde_json::from_str::<ServiceName>("\"Weather Feed\"").is_err());
        assert!(serde_json::from_str::<MissionGuid>("\"not-a-uuid\"").is_err());
    }

    #[test]
    fn debug_shows_the_value() {
        assert_eq!(
            format!("{:?}", ServiceName::parse("weather").unwrap()),
            "ServiceName(weather)"
        );
        assert_eq!(
            format!(
                "{:?}",
                MissionGuid::parse("0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f").unwrap()
            ),
            "MissionGuid(0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f)"
        );
    }
}
