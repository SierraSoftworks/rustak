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

pub use jiff::{SignedDuration, civil};

/// The chrono `TimeDelta` conveniences that have no direct jiff spelling.
pub trait DurationExt {
    /// The duration as a `std::time::Duration`; an error if it is negative.
    fn to_std(self) -> Result<std::time::Duration, jiff::Error>;
    /// Whole milliseconds, saturating at the `i64` range.
    fn whole_millis(self) -> i64;
}

impl DurationExt for SignedDuration {
    fn to_std(self) -> Result<std::time::Duration, jiff::Error> {
        std::time::Duration::try_from(self)
    }

    fn whole_millis(self) -> i64 {
        i64::try_from(self.as_millis()).unwrap_or(if self.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        })
    }
}

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

    /// The earliest representable instant (year -9999).
    pub const MIN: Self = Self(jiff::Timestamp::MIN);

    /// The latest representable instant (year 9999).
    pub const MAX: Self = Self(jiff::Timestamp::MAX);

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

    /// The UTC calendar date and wall-clock time of this instant.
    pub fn civil(self) -> jiff::civil::DateTime {
        jiff::tz::Offset::UTC.to_datetime(self.0)
    }

    /// The instant a UTC calendar date and time names; `None` when out of range.
    pub fn from_civil(datetime: jiff::civil::DateTime) -> Option<Self> {
        jiff::tz::Offset::UTC.to_timestamp(datetime).ok().map(Self)
    }

    /// `strftime`-style rendering in UTC (jiff's directives, a superset of the common chrono ones).
    pub fn strftime<'f, F: AsRef<[u8]> + ?Sized>(self, format: &'f F) -> impl fmt::Display + 'f {
        self.0.strftime(format)
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

impl Default for Timestamp {
    /// The Unix epoch, as chrono's `DateTime<Utc>::default()`.
    fn default() -> Self {
        Self::UNIX_EPOCH
    }
}

impl From<std::time::SystemTime> for Timestamp {
    /// Clamps to jiff's range, which spans years -9999 to 9999.
    fn from(value: std::time::SystemTime) -> Self {
        jiff::Timestamp::try_from(value)
            .map(Self)
            .unwrap_or_else(|_| {
                if value > std::time::UNIX_EPOCH {
                    Self(jiff::Timestamp::MAX)
                } else {
                    Self(jiff::Timestamp::MIN)
                }
            })
    }
}

impl From<Timestamp> for std::time::SystemTime {
    fn from(value: Timestamp) -> Self {
        let offset = value.duration_since(Timestamp::UNIX_EPOCH);
        let magnitude = offset.unsigned_abs();
        if offset.is_negative() {
            std::time::UNIX_EPOCH - magnitude
        } else {
            std::time::UNIX_EPOCH + magnitude
        }
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

impl Add<std::time::Duration> for Timestamp {
    type Output = Self;

    /// Panics on overflow, like chrono's `DateTime + std::time::Duration`.
    fn add(self, rhs: std::time::Duration) -> Self {
        Self(self.0.checked_add(rhs).expect("timestamp overflow"))
    }
}

impl AddAssign<std::time::Duration> for Timestamp {
    fn add_assign(&mut self, rhs: std::time::Duration) {
        *self = *self + rhs;
    }
}

impl Sub<std::time::Duration> for Timestamp {
    type Output = Self;

    /// Panics on overflow, like chrono's `DateTime - std::time::Duration`.
    fn sub(self, rhs: std::time::Duration) -> Self {
        Self(self.0.checked_sub(rhs).expect("timestamp overflow"))
    }
}

impl SubAssign<std::time::Duration> for Timestamp {
    fn sub_assign(&mut self, rhs: std::time::Duration) {
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

        let mut moving = start;
        moving += SignedDuration::from_mins(5);
        moving -= SignedDuration::from_mins(5);
        assert_eq!(moving, start);

        assert_eq!(
            start.checked_add(SignedDuration::from_secs(1)),
            Timestamp::from_second(BASE + 1)
        );
        assert_eq!(
            start.checked_sub(SignedDuration::from_secs(1)),
            Timestamp::from_second(BASE - 1)
        );
        assert_eq!(Timestamp::UNIX_EPOCH.checked_sub(SignedDuration::MAX), None);
    }

    #[rstest]
    #[case(i64::MAX, 0)]
    #[case(0, 1_000_000_000)]
    fn out_of_range_constructors_return_none(#[case] second: i64, #[case] nanosecond: i32) {
        assert!(Timestamp::new(second, nanosecond).is_none());
    }

    #[test]
    fn conversions_round_trip() {
        let ours = Timestamp::from_millisecond(BASE * 1_000 + 123).unwrap();
        let jiff: jiff::Timestamp = ours.into();

        assert_eq!(Timestamp::from(jiff), ours);
        assert_eq!(ours.get(), jiff);
        assert_eq!(ours.as_millisecond(), BASE * 1_000 + 123);
        assert_eq!(format!("{ours:?}"), ours.to_string());
        assert!(Timestamp::from_millisecond(i64::MAX).is_none());
    }

    #[test]
    fn now_is_after_the_epoch() {
        assert!(Timestamp::now() > Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn invalid_input_is_reported() {
        let err = "nope".parse::<Timestamp>().unwrap_err();
        assert!(err.to_string().starts_with("invalid timestamp"));
        assert!(std::error::Error::source(&err).is_none());

        let json = serde_json::from_str::<Timestamp>("42").unwrap_err();
        assert!(json.to_string().contains("RFC 3339"));
        assert!(serde_json::from_str::<Timestamp>("\"nope\"").is_err());
    }

    #[rstest]
    #[case::positive(SignedDuration::from_millis(1_500), Some(1_500))]
    #[case::negative(SignedDuration::from_millis(-1_500), None)]
    fn duration_to_std(#[case] d: SignedDuration, #[case] millis: Option<u128>) {
        assert_eq!(d.to_std().ok().map(|s| s.as_millis()), millis);
    }

    #[rstest]
    #[case::small(SignedDuration::from_millis(-42), -42)]
    #[case::saturates_high(SignedDuration::MAX, i64::MAX)]
    #[case::saturates_low(SignedDuration::MIN, i64::MIN)]
    fn duration_whole_millis(#[case] d: SignedDuration, #[case] expected: i64) {
        assert_eq!(d.whole_millis(), expected);
    }

    #[test]
    fn civil_round_trip() {
        let ts = Timestamp::from_second(BASE).unwrap();
        let civil = ts.civil();
        assert_eq!(
            (civil.year(), civil.month(), civil.day(), civil.hour()),
            (2025, 9, 18, 10)
        );
        assert_eq!(Timestamp::from_civil(civil), Some(ts));
        assert_eq!(
            ts.strftime("%Y-%m-%d %H:%M").to_string(),
            "2025-09-18 10:00"
        );
    }

    #[test]
    fn default_is_epoch() {
        assert_eq!(Timestamp::default(), Timestamp::UNIX_EPOCH);
    }

    #[rstest]
    #[case::after_epoch(BASE)]
    #[case::before_epoch(-BASE)]
    fn system_time_round_trip(#[case] second: i64) {
        let ts = Timestamp::new(second, 250_000_000).unwrap();
        let st = std::time::SystemTime::from(ts);
        assert_eq!(Timestamp::from(st), ts);
    }

    #[test]
    fn system_time_clamps_out_of_range() {
        let far = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1 << 40);
        assert_eq!(Timestamp::from(far), Timestamp::MAX);
    }

    #[test]
    fn std_duration_arithmetic() {
        let start = Timestamp::from_second(BASE).unwrap();
        let step = std::time::Duration::from_secs(90);
        let mut ts = start + step;
        assert_eq!(ts.as_second(), BASE + 90);
        ts -= step;
        assert_eq!(ts, start);
        ts += step;
        assert_eq!(ts - step, start);
    }
}
