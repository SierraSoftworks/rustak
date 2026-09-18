//! The three date formats the Marti surface emits, and the time window every
//! history query is bounded by.
//!
//! # Why there is more than one formatter
//!
//! TAK Server uses at least three patterns, chosen per *field* rather than per
//! endpoint: padded-millisecond instants for almost everything, a bare
//! `yyyy-MM-dd` for a channel's creation date, and Java's `Date.toString()` for
//! the `Time` key of an Enterprise Sync metadata map. ATAK parses each with the
//! matching pattern and rejects a value in the wrong one, so a single "the date
//! format" helper would be wrong for two of the three.
//!
//! Milliseconds are always emitted with three digits. TAK's own pattern for
//! some of these fields is a single `S`, which Java renders unpadded — but a
//! three-digit value is a valid instance of that pattern to a Java parser, and
//! it is what ATAK's `SSS` parsers need. Where a client is known to require the
//! unpadded form (`clientEndPoints`'s `lastEventTime`), that endpoint says so
//! and uses [`cot_date_unpadded`].
//!
//! # Why parsing is lenient and formatting is not
//!
//! Inputs arrive from clients we do not control: ATAK sends padded millis,
//! CloudTAK sends whatever `Date.toISOString()` produced, and a script sends a
//! bare date. [`parse_date`] accepts all of them. What we *emit* is exactly one
//! spelling per field, because that is the half a client parses strictly.

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};

use super::error::MartiError;

/// The longest span a history query may cover.
///
/// A query for "everything since the epoch" would walk every segment file on
/// disk to answer one map refresh. The cap is applied silently — the response
/// is the most recent day of the window that was asked for — and
/// [`TimeWindow::capped`] records that it happened so a handler can say so.
pub const MAX_WINDOW_HOURS: i64 = 24;

/// A padded-millisecond instant: `2024-01-31T09:07:05.120Z`.
///
/// The default for every Marti timestamp. The `Z` is literal — we always render
/// in UTC rather than emitting a real offset, because ATAK's patterns end in a
/// quoted `'Z'` and would not parse `+00:00`.
pub fn cot_date(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The unpadded-millisecond spelling, for `clientEndPoints`'s `lastEventTime`.
///
/// One to three digits with no trailing zeros, which is what Java renders from
/// a single-`S` pattern. `contacts.md` calls this out as one of the few fields
/// that genuinely needs it rather than the padded form.
pub fn cot_date_unpadded(at: DateTime<Utc>) -> String {
    let padded = cot_date(at);
    let Some((head, tail)) = padded.split_once('.') else {
        return padded;
    };
    let millis = tail.trim_end_matches('Z');
    let trimmed = millis.trim_end_matches('0');
    let millis = if trimmed.is_empty() { "0" } else { trimmed };

    format!("{head}.{millis}Z")
}

/// A bare date: `2024-01-31`.
///
/// What a channel's `created` field carries. ATAK parses that field with a
/// date-only pattern and will not accept an instant there.
pub fn group_date(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

/// Java's `Date.toString()`: `Wed Jan 31 09:07:05 UTC 2024`.
///
/// The `Time` key of the Enterprise Sync metadata map is a Java `Date` that was
/// interpolated into a string rather than formatted, so this is what clients
/// have learned to read there. The zone abbreviation is always `UTC` because we
/// never run the server's formatting in a local zone.
pub fn java_date_string(at: DateTime<Utc>) -> String {
    at.format("%a %b %d %H:%M:%S UTC %Y").to_string()
}

/// Parses a timestamp a client sent, in any of the spellings they send.
///
/// Accepts RFC 3339 with or without milliseconds and with any offset, the
/// literal-`Z` forms ATAK emits, a bare `yyyy-MM-dd`, and epoch milliseconds as
/// digits — the last because ATAK's channel bodies send `created` as a number.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] naming the value, so that an operator reading
/// a client's failure can see what it sent.
pub fn parse_date(value: &str) -> Result<DateTime<Utc>, MartiError> {
    let value = value.trim();

    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }

    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f") {
        return Ok(naive.and_utc());
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
    }

    if let Ok(millis) = value.parse::<i64>()
        && let Some(at) = DateTime::from_timestamp_millis(millis)
    {
        return Ok(at);
    }

    Err(MartiError::InvalidRequest(format!(
        "{value} is not a timestamp"
    )))
}

/// The bounds of a history query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeWindow {
    /// The earliest instant included.
    pub start: DateTime<Utc>,
    /// The latest instant included.
    pub end: DateTime<Utc>,
    /// Whether the window asked for was longer than [`MAX_WINDOW_HOURS`] and
    /// has been narrowed to its most recent day.
    pub capped: bool,
}

impl TimeWindow {
    /// Resolves the `secago` / `start` / `end` trio every history query carries.
    ///
    /// An explicit `start` or `end` wins over `secago`, which is how both ATAK
    /// and CloudTAK expect the three to interact: `secago` is the convenience
    /// form, the pair is the precise one. With none of the three, the window is
    /// the most recent [`MAX_WINDOW_HOURS`].
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a negative `secago` (which would
    /// place the window in the future), for a timestamp that does not parse,
    /// and for an `end` that precedes its `start`.
    pub fn parse(
        secago: Option<i64>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Self, MartiError> {
        Self::parse_at(Utc::now(), secago, start, end)
    }

    /// [`parse`](Self::parse) against a fixed "now", so a test can assert the
    /// bounds rather than approximate them.
    ///
    /// # Errors
    ///
    /// As [`parse`](Self::parse).
    pub fn parse_at(
        now: DateTime<Utc>,
        secago: Option<i64>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Self, MartiError> {
        let cap = chrono::Duration::hours(MAX_WINDOW_HOURS);

        let (start, end) = match (start, end) {
            (None, None) => match secago {
                Some(seconds) if seconds < 0 => {
                    return Err(MartiError::InvalidRequest(
                        "secago cannot be negative".to_string(),
                    ));
                }
                Some(seconds) => (now - chrono::Duration::seconds(seconds), now),
                None => (now - cap, now),
            },
            (start, end) => {
                let end = end.map(parse_date).transpose()?.unwrap_or(now);
                let start = start.map(parse_date).transpose()?.unwrap_or(end - cap);

                (start, end)
            }
        };

        if end < start {
            return Err(MartiError::InvalidRequest(
                "end is before start".to_string(),
            ));
        }

        let capped = end - start > cap;
        let start = if capped { end - cap } else { start };

        Ok(Self { start, end, capped })
    }

    /// How long the resolved window covers.
    pub fn duration(&self) -> chrono::Duration {
        self.end - self.start
    }

    /// Whether an instant falls inside the window.
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        at >= self.start && at <= self.end
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    /// The instant every formatter assertion is written against.
    fn fixture() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 1, 31, 9, 7, 5)
            .single()
            .expect("a real instant")
            + chrono::Duration::milliseconds(120)
    }

    #[test]
    fn a_marti_instant_is_padded_to_three_digits_with_a_literal_z() {
        // ATAK's parsers are `…ss.SSS'Z'`; a real offset or a one-digit
        // fraction is refused by the pattern, not coerced.
        assert_eq!(cot_date(fixture()), "2024-01-31T09:07:05.120Z");

        let whole = Utc.with_ymd_and_hms(2024, 1, 31, 9, 7, 5).unwrap();
        assert_eq!(cot_date(whole), "2024-01-31T09:07:05.000Z");
    }

    #[test]
    fn the_client_endpoint_spelling_drops_the_trailing_zeros() {
        // `contacts.md` §2: `lastEventTime` is the unpadded form, and a padded
        // one there is the mistake this function exists to avoid.
        assert_eq!(cot_date_unpadded(fixture()), "2024-01-31T09:07:05.12Z");

        let whole = Utc.with_ymd_and_hms(2024, 1, 31, 9, 7, 5).unwrap();
        assert_eq!(cot_date_unpadded(whole), "2024-01-31T09:07:05.0Z");

        let one = whole + chrono::Duration::milliseconds(100);
        assert_eq!(cot_date_unpadded(one), "2024-01-31T09:07:05.1Z");
    }

    #[test]
    fn a_channel_creation_date_carries_no_time_at_all() {
        assert_eq!(group_date(fixture()), "2024-01-31");
    }

    #[test]
    fn the_metadata_time_key_is_a_java_date_rendered_as_a_string() {
        assert_eq!(java_date_string(fixture()), "Wed Jan 31 09:07:05 UTC 2024");
    }

    #[test]
    fn every_spelling_a_client_sends_parses_back_to_the_same_instant() {
        let expected = Utc.with_ymd_and_hms(2024, 1, 31, 9, 7, 5).unwrap();

        for value in [
            "2024-01-31T09:07:05Z",
            "2024-01-31T09:07:05.000Z",
            "2024-01-31T09:07:05.0Z",
            "2024-01-31T09:07:05+00:00",
            "  2024-01-31T09:07:05Z  ",
        ] {
            assert_eq!(parse_date(value).unwrap(), expected, "{value}");
        }

        assert_eq!(
            parse_date("2024-01-31T10:07:05+01:00").unwrap(),
            expected,
            "an offset is honoured rather than ignored",
        );
        assert_eq!(
            parse_date("2024-01-31").unwrap(),
            Utc.with_ymd_and_hms(2024, 1, 31, 0, 0, 0).unwrap(),
        );
        assert_eq!(
            parse_date(&expected.timestamp_millis().to_string()).unwrap(),
            expected,
            "ATAK sends a channel's `created` as epoch milliseconds",
        );
    }

    #[test]
    fn a_round_trip_through_our_own_formatter_parses() {
        let at = fixture();

        assert_eq!(parse_date(&cot_date(at)).unwrap(), at);
        assert_eq!(parse_date(&cot_date_unpadded(at)).unwrap(), at);
    }

    #[test]
    fn something_that_is_not_a_timestamp_names_itself_in_the_refusal() {
        let Err(err) = parse_date("yesterday") else {
            panic!("`yesterday` is not a timestamp");
        };

        assert!(err.message().contains("yesterday"), "{}", err.message());
    }

    #[test]
    fn nothing_at_all_is_the_most_recent_day() {
        let now = fixture();
        let window = TimeWindow::parse_at(now, None, None, None).unwrap();

        assert_eq!(window.end, now);
        assert_eq!(window.duration(), chrono::Duration::hours(24));
        assert!(!window.capped);
    }

    #[test]
    fn secago_counts_back_from_now() {
        let now = fixture();
        let window = TimeWindow::parse_at(now, Some(600), None, None).unwrap();

        assert_eq!(window.end, now);
        assert_eq!(window.start, now - chrono::Duration::seconds(600));
        assert!(!window.capped);
        assert!(window.contains(now - chrono::Duration::seconds(1)));
        assert!(!window.contains(now - chrono::Duration::seconds(601)));
    }

    #[test]
    fn a_negative_secago_is_refused_rather_than_read_as_the_future() {
        let Err(err) = TimeWindow::parse_at(fixture(), Some(-1), None, None) else {
            panic!("a negative secago should be refused");
        };

        assert_eq!(err.status().as_u16(), 400);
    }

    #[test]
    fn an_explicit_pair_wins_over_secago() {
        // `secago` is the convenience form; a client that sent both meant the
        // precise one.
        let now = fixture();
        let window = TimeWindow::parse_at(
            now,
            Some(600),
            Some("2024-01-31T00:00:00Z"),
            Some("2024-01-31T06:00:00Z"),
        )
        .unwrap();

        assert_eq!(window.duration(), chrono::Duration::hours(6));
        assert_eq!(
            window.start,
            Utc.with_ymd_and_hms(2024, 1, 31, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn one_half_of_the_pair_takes_the_other_from_the_default() {
        let now = fixture();

        let from_start =
            TimeWindow::parse_at(now, None, Some("2024-01-31T06:00:00Z"), None).unwrap();
        assert_eq!(from_start.end, now);

        let to_end = TimeWindow::parse_at(now, None, None, Some("2024-01-31T06:00:00Z")).unwrap();
        assert_eq!(to_end.duration(), chrono::Duration::hours(24));
        assert_eq!(
            to_end.end,
            Utc.with_ymd_and_hms(2024, 1, 31, 6, 0, 0).unwrap()
        );
    }

    #[test]
    fn a_window_longer_than_a_day_is_narrowed_and_says_so() {
        // Silently answering with a day of a week-long query would look like a
        // gap in the history; the flag is what lets the handler explain it.
        let now = fixture();
        let window = TimeWindow::parse_at(
            now,
            None,
            Some("2024-01-01T00:00:00Z"),
            Some("2024-01-31T00:00:00Z"),
        )
        .unwrap();

        assert!(window.capped);
        assert_eq!(window.duration(), chrono::Duration::hours(24));
        assert_eq!(
            window.end,
            Utc.with_ymd_and_hms(2024, 1, 31, 0, 0, 0).unwrap(),
            "the day kept is the most recent one",
        );
    }

    #[test]
    fn a_secago_longer_than_a_day_is_capped_the_same_way() {
        let now = fixture();
        let window = TimeWindow::parse_at(now, Some(7 * 24 * 3600), None, None).unwrap();

        assert!(window.capped);
        assert_eq!(window.duration(), chrono::Duration::hours(24));
    }

    #[test]
    fn a_window_that_ends_before_it_starts_is_refused() {
        let Err(err) = TimeWindow::parse_at(
            fixture(),
            None,
            Some("2024-01-31T06:00:00Z"),
            Some("2024-01-31T00:00:00Z"),
        ) else {
            panic!("an inverted window should be refused");
        };

        assert_eq!(err.status().as_u16(), 400);
    }

    #[test]
    fn a_timestamp_that_does_not_parse_refuses_the_whole_query() {
        assert!(TimeWindow::parse_at(fixture(), None, Some("soon"), None).is_err());
        assert!(TimeWindow::parse_at(fixture(), None, None, Some("soon")).is_err());
    }
}
