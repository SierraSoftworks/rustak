//! Durations in a configuration file, written the way a person would write one.
//!
//! # Why this module exists
//!
//! [`chrono`] ships a family of serde adapters for putting a *timestamp* on the
//! wire in whatever unit the other end expects ([`chrono::serde::ts_seconds`]
//! and friends) and nothing at all for [`chrono::Duration`]: `chrono::serde` is
//! a re-export of its `datetime::serde` module. What `Duration` does have is a
//! `Serialize`/`Deserialize` pair that writes a `(seconds, nanoseconds)` tuple
//! — `[300, 0]` for five minutes — which is a fine thing for one program to
//! hand another and an impossible thing to put in a file somebody edits.
//!
//! automate solved the same problem with a whole number of minutes, because
//! every duration it stored was an answer to "how long should we wait". rustak's
//! durations span six orders of magnitude in the same file — a 90-second stream
//! idle timeout next to a 3650-day CA validity — so a single fixed unit would
//! make one end of that range unreadable. The unit goes in the value instead:
//! `"90s"`, `"5m"`, `"1h"`, `"3650d"`.
//!
//! # The accepted forms
//!
//! | Written | Means |
//! |---|---|
//! | `"45s"` | 45 seconds |
//! | `"15m"` | 15 minutes |
//! | `"12h"` | 12 hours |
//! | `"30d"` | 30 days |
//! | `90` | 90 seconds — a bare number is seconds |
//!
//! A bare number is accepted because `busy_timeout = 5` is a natural thing to
//! type and refusing it would teach nothing; it is never *written* that way, so
//! a file that round-trips through us comes back with its unit attached.
//!
//! # Negative and fractional spans are refused at both ends
//!
//! These are waiting periods, validity windows and retention horizons. "Keep
//! audit entries for minus ninety days" is not a thing we could do, and treating
//! it as zero would silently delete everything. A fractional value such as
//! `"1.5h"` is refused rather than rounded, for the same reason a fractional
//! number of minutes is refused elsewhere: a configuration that cannot be stored
//! as written should say so instead of quietly becoming a different one. Write
//! `"90m"`.
//!
//! The refusal applies when **writing** as well as when reading, so that
//! anything we are willing to emit can be read back. That matters because these
//! adapters also sit on values we write into the `settings` table and hand to
//! the admin UI: a span we would serialise but then refuse to parse is a
//! configuration the server can save and never load again.
//!
//! # Usage
//!
//! ```
//! # use serde::{Deserialize, Serialize};
//! #[derive(Debug, Serialize, Deserialize)]
//! struct StreamConfig {
//!     #[serde(with = "rustak_core::config::duration::humane")]
//!     idle_timeout: chrono::Duration,
//!
//!     #[serde(default, with = "rustak_core::config::duration::humane_option")]
//!     drain_timeout: Option<chrono::Duration>,
//! }
//!
//! let parsed: StreamConfig = toml::from_str("idle_timeout = \"90s\"").unwrap();
//! assert_eq!(parsed.idle_timeout, chrono::Duration::seconds(90));
//! assert_eq!(parsed.drain_timeout, None);
//! ```

use std::fmt;

use serde::de::{self, Unexpected, Visitor};

/// The refusal shown for a negative span, at both ends of the wire.
const NEGATIVE: &str =
    "A duration cannot be negative; give a span such as \"90s\", \"15m\", \"12h\" or \"30d\".";

/// The refusal shown for a span too large for [`chrono::Duration`] to hold.
const TOO_LARGE: &str = "That is longer than we could ever represent; give a smaller duration.";

/// The refusal shown for a fractional span.
const FRACTIONAL: &str =
    "A duration must be a whole number of its unit; write \"90m\" rather than \"1.5h\".";

/// Seconds per accepted unit suffix, longest-lived first so that
/// [`format`] picks the largest unit a span divides evenly into.
const UNITS: &[(char, i64)] = &[('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)];

/// Parses one of the forms documented at the [module level](self).
///
/// Returns the refusal text on failure so that both the serde adapters and any
/// caller validating a value by hand report the same wording.
pub fn parse(text: &str) -> Result<chrono::Duration, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(
            "A duration cannot be blank; give a span such as \"90s\", \"15m\", \"12h\" or \"30d\"."
                .to_string(),
        );
    }

    let (digits, seconds_per_unit) = match UNITS.iter().find(|(unit, _)| text.ends_with(*unit)) {
        Some((unit, seconds)) => (&text[..text.len() - unit.len_utf8()], *seconds),
        // A bare number is seconds; see the module documentation.
        None => (text, 1),
    };

    if digits.contains('.') {
        return Err(FRACTIONAL.to_string());
    }

    let count: i64 = digits.trim().parse().map_err(|_| {
        format!(
            "We could not read '{text}' as a duration; give a span such as \"90s\", \"15m\", \"12h\" or \"30d\"."
        )
    })?;

    from_seconds(count.checked_mul(seconds_per_unit).ok_or(TOO_LARGE)?)
}

/// Rebuilds a duration from a whole number of seconds, refusing the negative
/// and unrepresentable spans documented at the module level.
fn from_seconds(seconds: i64) -> Result<chrono::Duration, String> {
    if seconds < 0 {
        return Err(NEGATIVE.to_string());
    }

    chrono::Duration::try_seconds(seconds).ok_or_else(|| TOO_LARGE.to_string())
}

/// Renders a duration in the largest unit it divides evenly into.
///
/// # Errors
///
/// Refuses a negative span, and one carrying sub-second precision that the
/// written form cannot hold — see the [module documentation](self) for why a
/// value we cannot read back is one we decline to write.
pub fn format(value: chrono::Duration) -> Result<String, String> {
    if value < chrono::Duration::zero() {
        return Err(NEGATIVE.to_string());
    }

    if value.subsec_nanos() != 0 {
        return Err(FRACTIONAL.to_string());
    }

    let seconds = value.num_seconds();
    if seconds == 0 {
        return Ok("0s".to_string());
    }

    for (unit, per_unit) in UNITS {
        if seconds % per_unit == 0 {
            return Ok(format!("{}{unit}", seconds / per_unit));
        }
    }

    // Unreachable in practice: the last unit is seconds, which divides
    // everything. Kept as a total function rather than an `unwrap`.
    Ok(format!("{seconds}s"))
}

/// Accepts every form documented at the [module level](self) for one field.
struct HumaneVisitor;

impl Visitor<'_> for HumaneVisitor {
    type Value = chrono::Duration;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a duration such as \"90s\", \"15m\", \"12h\" or \"30d\", or a whole number of seconds",
        )
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        parse(value).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        from_seconds(value).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        let seconds = i64::try_from(value).map_err(|_| E::custom(TOO_LARGE))?;
        from_seconds(seconds).map_err(E::custom)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        // Named explicitly rather than left to the "invalid type" default, so
        // that `busy_timeout = 1.5` is told what to write instead.
        Err(E::invalid_value(Unexpected::Float(value), &FRACTIONAL))
    }
}

/// A [`chrono::Duration`] written the way a person would write one.
///
/// ```
/// # use serde::Deserialize;
/// #[derive(Deserialize)]
/// struct Retention {
///     #[serde(with = "rustak_core::config::duration::humane")]
///     audit: chrono::Duration,
/// }
///
/// let retention: Retention = toml::from_str(r#"audit = "90d""#).unwrap();
/// assert_eq!(retention.audit, chrono::Duration::days(90));
/// ```
pub mod humane {
    use serde::{Deserializer, Serializer};

    /// Writes the duration in the largest unit it divides evenly into.
    ///
    /// # Errors
    ///
    /// Refuses a negative or sub-second span; see the
    /// [module documentation](super).
    pub fn serialize<S: Serializer>(
        value: &chrono::Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let text = super::format(*value).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&text)
    }

    /// Reads any of the forms documented at the [module level](super).
    ///
    /// # Errors
    ///
    /// Refuses a blank, negative, fractional or unrepresentable span.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<chrono::Duration, D::Error> {
        deserializer.deserialize_any(super::HumaneVisitor)
    }
}

/// An `Option<`[`chrono::Duration`]`>`, or absent.
///
/// `#[serde(with = ...)]` makes a field mandatory even when its type is an
/// `Option` — serde's "a missing `Option` field is `None`" shortcut applies only
/// to fields it deserializes itself — so pair this with `#[serde(default)]` on
/// any key that is allowed to be left out of the file:
///
/// ```
/// # use serde::Deserialize;
/// #[derive(Deserialize)]
/// struct Acme {
///     #[serde(default, with = "rustak_core::config::duration::humane_option")]
///     renew_before: Option<chrono::Duration>,
/// }
///
/// let absent: Acme = toml::from_str("").unwrap();
/// assert_eq!(absent.renew_before, None);
///
/// let present: Acme = toml::from_str(r#"renew_before = "30d""#).unwrap();
/// assert_eq!(present.renew_before, Some(chrono::Duration::days(30)));
/// ```
pub mod humane_option {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes the duration as [`humane`](super::humane) does, or `null`.
    ///
    /// # Errors
    ///
    /// Refuses a negative or sub-second span; see the
    /// [module documentation](super).
    pub fn serialize<S: Serializer>(
        value: &Option<chrono::Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => {
                let text = super::format(*duration).map_err(serde::ser::Error::custom)?;
                serializer.serialize_some(&text)
            }
            None => serializer.serialize_none(),
        }
    }

    /// Reads any of the forms documented at the [module level](super), or a
    /// `null`/absent value as [`None`].
    ///
    /// # Errors
    ///
    /// Refuses a blank, negative, fractional or unrepresentable span.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<chrono::Duration>, D::Error> {
        Option::<Humane>::deserialize(deserializer).map(|held| held.map(|Humane(span)| span))
    }

    /// Carries [`super::HumaneVisitor`] through serde's `Option` handling,
    /// which needs a type to deserialize rather than a bare visitor.
    struct Humane(chrono::Duration);

    impl<'de> Deserialize<'de> for Humane {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(super::HumaneVisitor).map(Self)
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde::{Deserialize, Serialize};

    /// Stands in for a configuration section holding a mandatory span.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Section {
        #[serde(with = "super::humane")]
        span: chrono::Duration,
    }

    /// Stands in for a section whose span may be absent entirely.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct MaybeSection {
        #[serde(default, with = "super::humane_option")]
        span: Option<chrono::Duration>,
    }

    #[rstest]
    #[case("45s", chrono::Duration::seconds(45))]
    #[case("15m", chrono::Duration::minutes(15))]
    #[case("12h", chrono::Duration::hours(12))]
    #[case("30d", chrono::Duration::days(30))]
    #[case("3650d", chrono::Duration::days(3650))]
    #[case("0s", chrono::Duration::zero())]
    fn every_unit_in_the_documented_table_is_understood(
        #[case] written: &str,
        #[case] expected: chrono::Duration,
    ) {
        let section: Section =
            toml::from_str(&format!("span = \"{written}\"")).expect("{written} should parse");

        assert_eq!(section.span, expected);
    }

    #[test]
    fn a_bare_number_is_read_as_seconds() {
        // `busy_timeout = 5` is a natural thing to type and refusing it would
        // teach nothing, so it is accepted even though we never write it.
        let section: Section = toml::from_str("span = 5").unwrap();

        assert_eq!(section.span, chrono::Duration::seconds(5));
    }

    #[test]
    fn a_span_is_written_in_the_largest_unit_it_divides_evenly_into() {
        // The point of the module is the shape of the written value, not merely
        // that it survives a round trip — a round trip would still pass if we
        // emitted chrono's `[seconds, nanos]` pair, which is exactly the form
        // nobody can type. So assert on the text.
        for (span, expected) in [
            (chrono::Duration::days(30), "30d"),
            (chrono::Duration::hours(12), "12h"),
            (chrono::Duration::minutes(15), "15m"),
            (chrono::Duration::seconds(90), "90s"),
            (chrono::Duration::zero(), "0s"),
        ] {
            let written = toml::to_string(&Section { span }).unwrap();
            assert_eq!(written.trim(), format!("span = \"{expected}\""));
        }
    }

    #[test]
    fn anything_we_are_willing_to_write_can_be_read_back() {
        // The property the write-side refusals exist to protect, stated
        // directly: for every span that encodes at all, decoding succeeds and
        // gives back what went in.
        for span in [
            chrono::Duration::zero(),
            chrono::Duration::seconds(1),
            chrono::Duration::seconds(90),
            chrono::Duration::minutes(15),
            chrono::Duration::hours(12),
            chrono::Duration::days(3650),
        ] {
            let original = Section { span };
            let written =
                toml::to_string(&original).unwrap_or_else(|e| panic!("{span} should encode: {e}"));
            let read: Section = toml::from_str(&written)
                .unwrap_or_else(|e| panic!("{span} should decode from {written:?}: {e}"));

            assert_eq!(read, original);
        }
    }

    #[test]
    fn an_optional_span_round_trips_in_both_of_its_states() {
        for original in [
            MaybeSection {
                span: Some(chrono::Duration::days(30)),
            },
            MaybeSection { span: None },
        ] {
            let written = toml::to_string(&original).unwrap();
            let read: MaybeSection = toml::from_str(&written).unwrap();

            assert_eq!(read, original);
        }
    }

    #[test]
    fn an_absent_optional_span_reads_as_nothing_at_all() {
        // `#[serde(with = ...)]` makes a field mandatory even when it is an
        // `Option`, so the `#[serde(default)]` that restores the usual
        // behaviour is load-bearing — without it every key documented as
        // optional would become required.
        let read: MaybeSection = toml::from_str("").unwrap();

        assert_eq!(read.span, None);
    }

    #[rstest]
    #[case(r#"span = "-5m""#, "negative")]
    #[case("span = -5", "negative")]
    #[case(r#"span = "1.5h""#, "whole number")]
    #[case("span = 1.5", "whole number")]
    #[case(r#"span = """#, "blank")]
    #[case(r#"span = "5 fortnights""#, "could not read")]
    fn a_span_we_cannot_store_as_written_is_refused_and_says_why(
        #[case] written: &str,
        #[case] expected: &str,
    ) {
        let Err(err) = toml::from_str::<Section>(written) else {
            panic!("{written} should not load");
        };

        assert!(
            err.to_string().contains(expected),
            "expected {expected:?} in: {err}",
        );
    }

    #[test]
    fn a_span_too_large_to_hold_is_refused_by_name() {
        // `i64` days overflows `chrono::Duration` long before it overflows
        // itself, so the bound has to be checked rather than assumed.
        let Err(err) = toml::from_str::<Section>(&format!("span = \"{}d\"", i64::MAX)) else {
            panic!("an unrepresentable span should not load");
        };

        assert!(err.to_string().contains("smaller duration"), "{err}");
    }

    #[test]
    fn a_negative_span_is_refused_when_written_as_well_as_when_read() {
        // Refusing on read alone would let the server save a setting it then
        // declines to load, which is a configuration that cannot be recovered
        // without hand-editing the database.
        let Err(err) = toml::to_string(&Section {
            span: chrono::Duration::minutes(-5),
        }) else {
            panic!("a negative span should not be written");
        };

        assert!(err.to_string().contains("negative"), "{err}");
    }

    #[test]
    fn sub_second_precision_is_refused_on_the_way_out_rather_than_truncated() {
        // Truncating would write a value that reads back as a different span,
        // breaking the round-trip property asserted above. No configuration key
        // in rustak is finer than a second, so refusing costs nothing.
        let Err(err) = super::format(chrono::Duration::milliseconds(1_500)) else {
            panic!("a sub-second span should not be written");
        };

        assert!(err.contains("whole number"), "{err}");
    }
}
