//! Boilerplate shared by the validated string newtypes in this module.
//!
//! Every identity newtype here is a `String` that has been through a `parse`
//! function, so the interesting part of each type is its rules and its error —
//! not the dozen trait implementations that make it behave like a string. This
//! macro supplies those, leaving each module to define only what it validates.
//!
//! # Why deserialisation validates
//!
//! These types arrive from two directions: a browser (or an ATAK client) via
//! JSON, and our own database. The wire is untrusted, so `Deserialize` runs the
//! same rules as `parse` and a malformed value is rejected at the edge rather
//! than somewhere deeper. A stored value written under older, looser rules
//! would fail that check, so every type also offers `from_storage`, which
//! normalises but does not validate. The storage layer uses it; nothing that
//! reads the network does.

/// Implements the string-like traits, plus validating serde, for a newtype
/// whose single field is a `String` and which offers `parse` and `from_storage`.
macro_rules! string_newtype {
    ($name:ident, $error:ty, $expecting:literal) => {
        impl $name {
            #[doc = concat!("The value, as ", $expecting, ".")]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Unwraps the newtype, yielding the string it guards.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::core::fmt::Debug for $name {
            /// Renders the value rather than the tuple, since the value is what
            /// appears in URLs, log lines and the UI.
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl ::core::convert::AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $error;

            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            /// Applies the type's own rules, so a malformed value coming off the
            /// wire is refused where it arrives.
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                struct Visitor;

                impl ::serde::de::Visitor<'_> for Visitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                        f.write_str($expecting)
                    }

                    fn visit_str<E: ::serde::de::Error>(
                        self,
                        value: &str,
                    ) -> ::core::result::Result<$name, E> {
                        <$name>::parse(value).map_err(::serde::de::Error::custom)
                    }
                }

                deserializer.deserialize_str(Visitor)
            }
        }
    };
}

pub(crate) use string_newtype;
