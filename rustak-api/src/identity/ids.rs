//! Typed identifiers for the rows the server stores.
//!
//! Every one of these is a SQLite `INTEGER PRIMARY KEY`, so they are all the
//! same thing at runtime: an `i64`. Giving each table its own type costs
//! nothing and means a device identifier cannot be passed where a user
//! identifier belongs, which is the kind of mistake that otherwise compiles and
//! then silently reads the wrong row.
//!
//! Row identifiers are internal. They are stable, they are handed to the UI so
//! it can address a row it just listed, and they are never secrets — anything
//! that must be unguessable carries its own token.

use core::fmt;
use core::str::FromStr;

/// Defines a set of row-identifier newtypes over `i64`.
macro_rules! define_id {
    ($(
        $(#[$doc:meta])*
        $name:ident
    ),+ $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(
                Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
                serde::Serialize, serde::Deserialize,
            )]
            #[repr(transparent)]
            #[serde(transparent)]
            pub struct $name(i64);

            impl $name {
                /// Wraps a row identifier read back from the database.
                pub const fn new(value: i64) -> Self {
                    Self(value)
                }

                /// The underlying row identifier, for binding to a statement.
                pub const fn get(self) -> i64 {
                    self.0
                }
            }

            impl From<i64> for $name {
                fn from(value: i64) -> Self {
                    Self(value)
                }
            }

            impl From<$name> for i64 {
                fn from(value: $name) -> Self {
                    value.0
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    fmt::Display::fmt(&self.0, f)
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "{}({})", stringify!($name), self.0)
                }
            }

            impl FromStr for $name {
                type Err = core::num::ParseIntError;

                /// Parses the identifier out of a path segment.
                fn from_str(value: &str) -> Result<Self, Self::Err> {
                    Ok(Self(value.trim().parse()?))
                }
            }
        )+
    };
}

define_id!(
    /// A row in `users`.
    UserId,
    /// A row in `devices`: one enrolled client.
    DeviceId,
    /// A row in `groups`: one channel.
    GroupId,
    /// A row in `certificates`.
    CertificateId,
    /// A row in `credentials`.
    CredentialId,
    /// A row in `passkeys`.
    ///
    /// Passkeys live in their own table rather than in `credentials`, because
    /// what is stored is a public key rather than the hash of a secret.
    PasskeyId,
    /// A row in `services`: one registered sidecar.
    ServiceId,
    /// A row in `missions`.
    MissionId,
    /// A row in `resources`: one stored file or data package.
    ResourceId,
    /// A row in `profiles`: one enrolment or configuration profile.
    ProfileId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_numbers_on_the_wire() {
        let id = UserId::new(42);

        assert_eq!(serde_json::to_string(&id).unwrap(), "42");
        assert_eq!(serde_json::from_str::<UserId>("42").unwrap(), id);
        assert_eq!(id.get(), 42);
    }

    #[test]
    fn every_identifier_round_trips_through_serde() {
        macro_rules! assert_round_trips {
            ($($name:ident),+ $(,)?) => {$({
                let id = $name::from(7i64);
                let json = serde_json::to_string(&id).unwrap();

                assert_eq!(json, "7", concat!(stringify!($name), " should be a number"));
                assert_eq!(serde_json::from_str::<$name>(&json).unwrap(), id);
                assert_eq!(i64::from(id), 7);
            })+};
        }

        assert_round_trips!(
            UserId,
            DeviceId,
            GroupId,
            CertificateId,
            CredentialId,
            PasskeyId,
            ServiceId,
            MissionId,
            ResourceId,
            ProfileId,
        );
    }

    #[test]
    fn identifiers_parse_out_of_a_path_segment() {
        assert_eq!("13".parse::<DeviceId>().unwrap(), DeviceId::new(13));
        assert_eq!(" 13 ".parse::<DeviceId>().unwrap(), DeviceId::new(13));
        assert!("thirteen".parse::<DeviceId>().is_err());
    }

    #[test]
    fn debug_names_the_table_the_row_is_in() {
        assert_eq!(format!("{:?}", GroupId::new(1)), "GroupId(1)");
        assert_eq!(format!("{}", GroupId::new(1)), "1");
    }
}
