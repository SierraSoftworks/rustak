//! CoT timestamps.
//!
//! Every time on the wire is an ISO-8601 instant in UTC. TAK Server itself
//! formats to *second* resolution and parses both, while the protobuf encoding
//! carries milliseconds; rustak keeps **milliseconds everywhere** and formats
//! with three fractional digits, which every client we have looked at accepts.

use std::fmt;
use std::ops::{Add, Sub};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

use crate::error::BadTime;

/// A CoT instant: milliseconds since the Unix epoch, UTC.
///
/// Ordering is chronological, so `stale <= now` is the staleness test.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CotTime(i64);

impl CotTime {
    /// The Unix epoch, `1970-01-01T00:00:00.000Z`.
    pub const EPOCH: Self = Self(0);

    /// The current wall-clock time, truncated to milliseconds.
    #[must_use]
    pub fn now() -> Self {
        Self(Utc::now().timestamp_millis())
    }

    /// Wraps a millisecond Unix timestamp.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The millisecond Unix timestamp.
    #[must_use]
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// Parses a CoT time attribute.
    ///
    /// Accepts `YYYY-MM-DDTHH:MM:SS[.fff…]Z` with one to nine fractional
    /// digits, and the equivalent numeric offsets (`+00:00`, `-05:00`).
    /// Sub-millisecond digits are truncated. A bare
    /// `YYYY-MM-DDTHH:MM:SS[.fff…]` with no zone is read as UTC, because some
    /// clients omit the `Z`.
    ///
    /// # Errors
    ///
    /// Returns [`BadTime`] when the value is not an ISO-8601 instant, or when
    /// it lies outside the range representable in milliseconds.
    pub fn parse(value: &str) -> Result<Self, BadTime> {
        let text = value.trim();
        DateTime::parse_from_rfc3339(text)
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|_| assume_utc(text))
            .map(|dt| Self(dt.timestamp_millis()))
            .map_err(|()| BadTime {
                value: value.to_owned(),
            })
    }

    /// The instant `after` later than this one, saturating at the i64 bounds.
    #[must_use]
    pub fn stale_after(self, after: Duration) -> Self {
        Self(self.0.saturating_add(millis_of(after)))
    }

    /// This instant as a `chrono` timestamp, or [`None`] when the millisecond
    /// value is outside the calendar range `chrono` can represent.
    #[must_use]
    pub fn to_datetime(self) -> Option<DateTime<Utc>> {
        Utc.timestamp_millis_opt(self.0).single()
    }

    /// Wraps a `chrono` timestamp, truncating to milliseconds.
    #[must_use]
    pub fn from_datetime(value: DateTime<Utc>) -> Self {
        Self(value.timestamp_millis())
    }
}

/// Reads a zone-less ISO-8601 local time as UTC.
fn assume_utc(text: &str) -> Result<DateTime<Utc>, ()> {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|naive| naive.and_utc())
        .map_err(|_| ())
}

/// A [`Duration`] as whole milliseconds, clamped to [`i64`].
fn millis_of(value: Duration) -> i64 {
    i64::try_from(value.as_millis()).unwrap_or(i64::MAX)
}

impl fmt::Display for CotTime {
    /// Formats as `YYYY-MM-DDTHH:MM:SS.sssZ`.
    ///
    /// Times outside the calendar range fall back to the raw millisecond
    /// count so that a `Display` impl can never panic or lose the value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_datetime() {
            Some(dt) => f.write_str(&dt.to_rfc3339_opts(SecondsFormat::Millis, true)),
            None => write!(f, "{}", self.0),
        }
    }
}

impl fmt::Debug for CotTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CotTime({self})")
    }
}

impl Add<Duration> for CotTime {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        self.stale_after(rhs)
    }
}

impl Sub<CotTime> for CotTime {
    type Output = i64;

    /// The signed millisecond gap between two instants.
    fn sub(self, rhs: Self) -> i64 {
        self.0.saturating_sub(rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("2026-09-17T12:00:00Z", 1_789_646_400_000)]
    #[case("2026-09-17T12:00:00.250Z", 1_789_646_400_250)]
    #[case("2026-09-17T12:00:00.250123456Z", 1_789_646_400_250)]
    #[case("2026-09-17T12:00:00.2Z", 1_789_646_400_200)]
    #[case("2026-09-17T12:00:00+00:00", 1_789_646_400_000)]
    #[case("2026-09-17T07:00:00-05:00", 1_789_646_400_000)]
    #[case("2026-09-17T12:00:00", 1_789_646_400_000)]
    fn parses_every_shape_tak_clients_emit(#[case] text: &str, #[case] millis: i64) {
        assert_eq!(CotTime::parse(text), Ok(CotTime::from_millis(millis)));
    }

    #[rstest]
    #[case("")]
    #[case("soon")]
    #[case("2026-09-17")]
    #[case("2026-13-17T12:00:00Z")]
    #[case("1789041600000")]
    fn rejects_values_that_are_not_cot_times(#[case] text: &str) {
        assert_eq!(
            CotTime::parse(text),
            Err(BadTime {
                value: text.to_owned()
            })
        );
    }

    #[test]
    fn display_always_carries_three_fractional_digits() {
        assert_eq!(
            CotTime::from_millis(1_789_646_400_000).to_string(),
            "2026-09-17T12:00:00.000Z"
        );
        assert_eq!(
            CotTime::from_millis(1_789_646_400_070).to_string(),
            "2026-09-17T12:00:00.070Z"
        );
    }

    #[test]
    fn display_and_parse_round_trip_at_millisecond_resolution() {
        let time = CotTime::from_millis(1_789_646_400_123);
        assert_eq!(CotTime::parse(&time.to_string()), Ok(time));
    }

    #[test]
    fn stale_after_adds_whole_milliseconds_and_saturates() {
        let time = CotTime::from_millis(1_000);
        assert_eq!(time.stale_after(Duration::from_secs(20)).millis(), 21_000);
        assert_eq!(
            time + Duration::from_millis(500),
            CotTime::from_millis(1_500)
        );
        assert_eq!(
            CotTime::from_millis(i64::MAX - 1)
                .stale_after(Duration::from_secs(1))
                .millis(),
            i64::MAX
        );
    }

    #[test]
    fn subtraction_yields_the_signed_gap_in_millis() {
        assert_eq!(
            CotTime::from_millis(2_500) - CotTime::from_millis(1_000),
            1_500
        );
        assert_eq!(
            CotTime::from_millis(1_000) - CotTime::from_millis(2_500),
            -1_500
        );
    }

    #[test]
    fn ordering_is_chronological() {
        assert!(CotTime::from_millis(1) < CotTime::from_millis(2));
        assert_eq!(CotTime::EPOCH.millis(), 0);
    }

    #[test]
    fn now_is_close_to_the_chrono_clock() {
        let gap = (CotTime::now() - CotTime::from_datetime(Utc::now())).abs();
        assert!(gap < 1_000, "clock drifted by {gap}ms");
    }

    #[test]
    fn debug_shows_the_iso_form() {
        assert_eq!(
            format!("{:?}", CotTime::from_millis(0)),
            "CotTime(1970-01-01T00:00:00.000Z)"
        );
    }
}
