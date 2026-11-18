//! The one instant type every rustak crate shares.
//!
//! [`Timestamp`] wraps [`jiff::Timestamp`] and replaces `chrono::DateTime<Utc>`
//! across the workspace (RUSTSEC-2026-0543: chrono is unmaintained). It exists
//! as a newtype rather than a bare re-export for one reason: the text it
//! produces must stay byte-for-byte what `chrono` produced, because that text
//! is on the wire (admin API JSON, CoT, Marti) and in fixtures.
//!
//! | Rendering | Shape | chrono equivalent |
//! |---|---|---|
//! | `Display`, serde, [`Precision::Auto`] | `2026-09-18T10:00:00.100Z` (0, 3, 6 or 9 digits) | serde / `SecondsFormat::AutoSi` |
//! | [`Precision::Millis`] | `2026-09-18T10:00:00.100Z` (always 3) | `SecondsFormat::Millis` |
//! | [`Precision::Secs`] | `2026-09-18T10:00:00Z` | `SecondsFormat::Secs` |
//!
//! jiff's own rendering trims trailing zeros (`.1Z`), which is valid RFC 3339
//! but not what peers and fixtures have seen so far.
//!
//! Parsing accepts any RFC 3339 / RFC 9557 instant with an offset, exactly as
//! `DateTime<Utc>`'s `Deserialize` did.

use std::{
    fmt,
    ops::{Add, AddAssign, Sub, SubAssign},
    str::FromStr,
};

pub use jiff::SignedDuration;

/// A UTC instant with nanosecond resolution.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(jiff::Timestamp);

/// How many fractional-second digits [`Timestamp::to_rfc3339`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// None: `…:00Z`. Sub-second digits are truncated.
    Secs,
    /// Exactly three: `…:00.000Z`. Sub-millisecond digits are truncated.
    Millis,
    /// The fewest of 0, 3, 6 or 9 that lose nothing.
    Auto,
}

impl Timestamp {
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: Self = Self(jiff::Timestamp::UNIX_EPOCH);

    /// The current instant.
    ///
    /// On `wasm32-unknown-unknown` this needs jiff's `js` feature, which the
    /// browser crate turns on.
    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    /// Seconds and nanoseconds since the epoch; `None` when out of range.
    pub fn new(second: i64, nanosecond: i32) -> Option<Self> {
        jiff::Timestamp::new(second, nanosecond).ok().map(Self)
    }

    /// Whole seconds since the epoch; `None` when out of range.
    pub fn from_second(second: i64) -> Option<Self> {
        jiff::Timestamp::from_second(second).ok().map(Self)
    }

    /// Milliseconds since the epoch; `None` when out of range.
    pub fn from_millisecond(millisecond: i64) -> Option<Self> {
        jiff::Timestamp::from_millisecond(millisecond)
            .ok()
            .map(Self)
    }

    /// Whole seconds since the epoch, rounded towards negative infinity like
    /// chrono's `timestamp()`.
    pub fn as_second(self) -> i64 {
        self.0.as_millisecond().div_euclid(1_000)
    }

    /// Milliseconds since the epoch, rounded towards negative infinity like
    /// chrono's `timestamp_millis()`.
    pub fn as_millisecond(self) -> i64 {
        self.0.as_microsecond().div_euclid(1_000)
    }

    /// Nanoseconds into the current second, always in `0..1_000_000_000`.
    pub fn subsec_nanosecond(self) -> u32 {
        (self.0.as_nanosecond().rem_euclid(1_000_000_000)) as u32
    }

    /// The wrapped jiff value, for anything this type does not expose.
    pub fn get(self) -> jiff::Timestamp {
        self.0
    }

    /// `self + duration`, or `None` on overflow.
    pub fn checked_add(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_add(duration).ok().map(Self)
    }

    /// `self - duration`, or `None` on overflow.
    pub fn checked_sub(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_sub(duration).ok().map(Self)
    }

    /// The time elapsed from `earlier` to `self` (negative if `earlier` is later).
    pub fn duration_since(self, earlier: Self) -> SignedDuration {
        self.0.duration_since(earlier.0)
    }

    /// RFC 3339 in UTC with a `Z` suffix, at the given precision.
    pub fn to_rfc3339(self, precision: Precision) -> String {
        let civil = jiff::tz::Offset::UTC.to_datetime(self.0);
        let mut text = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            civil.year(),
            civil.month(),
            civil.day(),
            civil.hour(),
            civil.minute(),
            civil.second(),
        );

        let nanos = self.subsec_nanosecond();
        match precision {
            Precision::Secs => {}
            Precision::Millis => text.push_str(&format!(".{:03}", nanos / 1_000_000)),
            Precision::Auto if nanos == 0 => {}
            Precision::Auto if nanos.is_multiple_of(1_000_000) => {
                text.push_str(&format!(".{:03}", nanos / 1_000_000))
            }
            Precision::Auto if nanos.is_multiple_of(1_000) => {
                text.push_str(&format!(".{:06}", nanos / 1_000))
            }
            Precision::Auto => text.push_str(&format!(".{nanos:09}")),
        }

        text.push('Z');
        text
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339(Precision::Auto))
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The text was not an RFC 3339 instant with an offset.
#[derive(Debug)]
pub struct ParseError(jiff::Error);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid timestamp: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

impl FromStr for Timestamp {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self).map_err(ParseError)
    }
}

impl From<jiff::Timestamp> for Timestamp {
    fn from(value: jiff::Timestamp) -> Self {
        Self(value)
    }
}

impl From<Timestamp> for jiff::Timestamp {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl Add<SignedDuration> for Timestamp {
    type Output = Self;

    /// Panics on overflow, like chrono's `DateTime + TimeDelta`.
    fn add(self, rhs: SignedDuration) -> Self {
        Self(self.0 + rhs)
    }
}

impl AddAssign<SignedDuration> for Timestamp {
    fn add_assign(&mut self, rhs: SignedDuration) {
        *self = *self + rhs;
    }
}

impl Sub<SignedDuration> for Timestamp {
    type Output = Self;

    /// Panics on overflow, like chrono's `DateTime - TimeDelta`.
    fn sub(self, rhs: SignedDuration) -> Self {
        Self(self.0 - rhs)
    }
}

impl SubAssign<SignedDuration> for Timestamp {
    fn sub_assign(&mut self, rhs: SignedDuration) {
        *self = *self - rhs;
    }
}

impl Sub for Timestamp {
    type Output = SignedDuration;

    fn sub(self, rhs: Self) -> SignedDuration {
        self.duration_since(rhs)
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = Timestamp;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an RFC 3339 timestamp")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Timestamp, E> {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, SecondsFormat, Utc};
    use rstest::rstest;

    const BASE: i64 = 1_758_189_600; // 2025-09-18T10:00:00Z

    fn pair(second: i64, nanosecond: u32) -> (Timestamp, DateTime<Utc>) {
        (
            Timestamp::new(second, nanosecond as i32).expect("in range"),
            DateTime::from_timestamp(second, nanosecond).expect("in range"),
        )
    }

    #[rstest]
    #[case(BASE, 0, "2025-09-18T10:00:00Z")]
    #[case(BASE, 100_000_000, "2025-09-18T10:00:00.100Z")]
    #[case(BASE, 123_400_000, "2025-09-18T10:00:00.123400Z")]
    #[case(BASE, 123_456_789, "2025-09-18T10:00:00.123456789Z")]
    #[case(BASE, 5, "2025-09-18T10:00:00.000000005Z")]
    #[case(0, 0, "1970-01-01T00:00:00Z")]
    #[case(-1, 500_000_000, "1969-12-31T23:59:59.500Z")]
    fn serde_matches_chrono(#[case] second: i64, #[case] nanosecond: u32, #[case] expected: &str) {
        let (ours, theirs) = pair(second, nanosecond);
        let json = serde_json::to_string(&ours).unwrap();

        assert_eq!(json, format!("\"{expected}\""));
        assert_eq!(json, serde_json::to_string(&theirs).unwrap());
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), ours);
    }

    #[rstest]
    #[case(Precision::Secs, SecondsFormat::Secs)]
    #[case(Precision::Millis, SecondsFormat::Millis)]
    #[case(Precision::Auto, SecondsFormat::AutoSi)]
    fn rfc3339_matches_chrono(#[case] ours: Precision, #[case] theirs: SecondsFormat) {
        for nanosecond in [
            0,
            1,
            999,
            1_000,
            999_999,
            1_000_000,
            120_000_000,
            999_999_999,
        ] {
            for second in [BASE, 0, -1, 4_102_444_799] {
                let (a, b) = pair(second, nanosecond);
                assert_eq!(
                    a.to_rfc3339(ours),
                    b.to_rfc3339_opts(theirs, true),
                    "{second}.{nanosecond}"
                );
            }
        }
    }

    #[rstest]
    #[case("2026-11-18T10:00:00+01:00")]
    #[case("2026-11-18T10:00:00.5z")]
    #[case("2026-11-18 10:00:00Z")]
    #[case("2026-11-18T10:00:00.123-05:30")]
    fn parses_what_chrono_parses(#[case] text: &str) {
        let ours: Timestamp = serde_json::from_str(&format!("\"{text}\"")).unwrap();
        let theirs: DateTime<Utc> = serde_json::from_str(&format!("\"{text}\"")).unwrap();

        assert_eq!(
            ours.to_rfc3339(Precision::Auto),
            theirs.to_rfc3339_opts(SecondsFormat::AutoSi, true)
        );
    }

    #[rstest]
    #[case("2026-11-18T10:00:00")]
    #[case("not a time")]
    #[case("")]
    fn rejects_instants_without_offset(#[case] text: &str) {
        assert!(text.parse::<Timestamp>().is_err());
    }

    #[test]
    fn epoch_accessors_match_chrono() {
        for (second, nanosecond) in [(BASE, 123_456_789), (-1, 500_000_000), (0, 0)] {
            let (a, b) = pair(second, nanosecond);
            assert_eq!(a.as_second(), b.timestamp());
            assert_eq!(a.as_millisecond(), b.timestamp_millis());
            assert_eq!(a.subsec_nanosecond(), b.timestamp_subsec_nanos());
        }
    }

    #[test]
    fn arithmetic_round_trips() {
        let start = Timestamp::from_second(BASE).unwrap();
        let later = start + SignedDuration::from_hours(2);

        assert_eq!(later - start, SignedDuration::from_hours(2));
        assert_eq!(later - SignedDuration::from_hours(2), start);
        assert!(later > start);
    }
}
