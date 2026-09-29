//! Reading how long an upstream asked us to wait.
//!
//! M9-12 found providers that state a delay in forms the RFC does not allow and
//! meant it anyway; this is the tolerant reading every HTTP feed shares, so a
//! delay that one plugin honours is a delay they all honour.

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::header::HeaderMap;

/// The header a `429` or `503` normally names its delay in.
pub const RETRY_AFTER: &str = "retry-after";

/// Reads the delay a refusal stated, in whichever form it stated it.
///
/// RFC 9110 §10.2.3 allows a whole number of seconds or an HTTP-date, and this
/// reads both — the date in all three of the spellings §5.6.7 obliges a
/// recipient to accept. It also reads **decimal seconds**, rounded up, which
/// the RFC does not allow and which rate limiters that think in milliseconds
/// send anyway: a provider that says `1.5` has plainly told us something, and
/// treating that as silence is how a stated delay gets ignored. `header` is
/// matched without regard to case, as header names are; it is usually
/// [`RETRY_AFTER`], and a provider's own header (OpenSky's) where it has one.
///
/// A value we cannot read, or a date in the past, answers [`None`] and the
/// caller treats the refusal as one that named no delay — being asked to wait
/// until a moment we cannot work out is not a reason to stop polling forever.
#[must_use]
pub fn retry_after(headers: &HeaderMap, header: &str) -> Option<Duration> {
    retry_after_at(headers, header, Utc::now())
}

/// [`retry_after`], with an HTTP-date judged against an instant of the
/// caller's choosing.
#[must_use]
pub fn retry_after_at(headers: &HeaderMap, header: &str, now: DateTime<Utc>) -> Option<Duration> {
    let value = headers.get(header)?.to_str().ok()?.trim();

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    if let Ok(seconds) = value.parse::<f64>() {
        // `inf`, `NaN` and a negative number all parse, and none is a delay.
        return (seconds.is_finite() && seconds >= 0.0)
            .then(|| Duration::try_from_secs_f64(seconds.ceil()).ok())
            .flatten();
    }

    let delay = (http_date(value)? - now).to_std().ok()?;

    // Whole seconds, rounded up: an HTTP-date has no fractions, our clock
    // does, and "wait 119s" for a date two minutes away is asking early.
    Some(Duration::from_secs(
        delay.as_secs() + u64::from(delay.subsec_nanos() > 0),
    ))
}

/// An HTTP-date in any of RFC 9110 §5.6.7's three forms: `Sun, 06 Nov 1994
/// 08:49:37 GMT`, the obsolete `Sunday, 06-Nov-94 08:49:37 GMT`, and
/// `asctime`'s `Sun Nov  6 08:49:37 1994`.
///
/// RFC 2822 first, because it reads a numeric zone, which an HTTP-date should
/// not carry and sometimes does. Then the three forms with the day name skipped
/// rather than checked: it says nothing the date does not, and a server that
/// gets it wrong has still told us when. Month names and `GMT` are read without
/// regard to case. A two-digit year is this century's, which is all a delay
/// can be.
fn http_date(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(at) = DateTime::parse_from_rfc2822(value) {
        return Some(at.with_timezone(&Utc));
    }

    let (_day, rest) = value.split_once([',', ' '])?;
    let rest = rest.trim();
    let rest = match rest
        .len()
        .checked_sub(3)
        .and_then(|at| rest.split_at_checked(at))
    {
        Some((date, zone)) if zone.eq_ignore_ascii_case("GMT") => date.trim_end(),
        _ => rest,
    };

    [
        "%d %b %Y %H:%M:%S",
        "%d-%b-%y %H:%M:%S",
        "%b %e %H:%M:%S %Y",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(rest, format).ok())
    .map(|at| at.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_str(value).unwrap());
        headers
    }

    /// The instant the date forms below are judged against.
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-22T01:00:00Z")
            .expect("an instant")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_retry_after_in_seconds_is_read_as_seconds() {
        assert_eq!(
            retry_after(&headers("30"), RETRY_AFTER),
            Some(Duration::from_secs(30)),
        );
        assert_eq!(
            retry_after(&headers(" 0 "), RETRY_AFTER),
            Some(Duration::ZERO),
            "padded, and nought is still something the provider said",
        );
    }

    #[test]
    fn a_retry_after_in_decimal_seconds_is_rounded_up_to_a_whole_one() {
        // Not in the RFC, and sent anyway by rate limiters that think in
        // milliseconds. Before M9-12 every one of these read as "no delay".
        for (written, expected) in [("9.5", 10), ("10.0", 10), ("0.001", 1), ("1e1", 10)] {
            assert_eq!(
                retry_after(&headers(written), RETRY_AFTER),
                Some(Duration::from_secs(expected)),
                "{written}",
            );
        }
    }

    #[test]
    fn a_number_that_is_not_a_delay_is_not_read_as_one() {
        for written in ["-1", "-0.5", "inf", "NaN", "1e400", "ten", "10s", ""] {
            assert_eq!(
                retry_after(&headers(written), RETRY_AFTER),
                None,
                "{written:?}",
            );
        }
    }

    #[test]
    fn a_retry_after_as_an_http_date_is_read_in_all_three_spellings() {
        // RFC 9110 §5.6.7: IMF-fixdate, the obsolete RFC 850 form, and asctime
        // — a recipient is obliged to read all three.
        for written in [
            "Tue, 22 Sep 2026 01:02:00 GMT",
            "Tuesday, 22-Sep-26 01:02:00 GMT",
            "Tue Sep 22 01:02:00 2026",
            "Tue, 22 Sep 2026 01:02:00 +0000",
        ] {
            assert_eq!(
                retry_after_at(&headers(written), RETRY_AFTER, now()),
                Some(Duration::from_secs(120)),
                "{written}",
            );
        }

        assert_eq!(
            retry_after_at(&headers("Sun Nov  1 00:00:00 2026"), RETRY_AFTER, now())
                .map(|delay| delay.as_secs() / 86_400),
            Some(39),
            "asctime pads a single-digit day with a space",
        );
    }

    #[test]
    fn a_retry_after_as_a_date_on_the_wall_clock_is_the_delay_until_then() {
        // What the ESB and FIRMS copies asserted before they were lifted.
        let at = Utc::now() + chrono::Duration::seconds(120);
        let delay =
            retry_after(&headers(&at.to_rfc2822()), RETRY_AFTER).expect("an HTTP date is a delay");

        assert!(
            delay > Duration::from_secs(100) && delay <= Duration::from_secs(120),
            "{delay:?}"
        );
    }

    #[test]
    fn neither_the_header_name_nor_the_date_is_case_sensitive() {
        let mut shouted = HeaderMap::new();
        shouted.insert(
            HeaderName::from_bytes(b"Retry-After").expect("a header name"),
            HeaderValue::from_static("tue, 22 sep 2026 01:02:00 gmt"),
        );

        for name in ["retry-after", "Retry-After", "RETRY-AFTER"] {
            assert_eq!(
                retry_after_at(&shouted, name, now()),
                Some(Duration::from_secs(120)),
                "{name}",
            );
        }
    }

    #[test]
    fn a_date_with_the_wrong_day_name_still_says_when() {
        // The twenty-second of September 2026 is a Tuesday.
        assert_eq!(
            retry_after_at(
                &headers("Mon, 22 Sep 2026 01:02:00 GMT"),
                RETRY_AFTER,
                now()
            ),
            Some(Duration::from_secs(120)),
        );
    }

    #[test]
    fn a_date_is_a_delay_in_whole_seconds_rounded_up() {
        let late = now() + chrono::Duration::milliseconds(400);

        assert_eq!(
            retry_after_at(&headers("Tue, 22 Sep 2026 01:02:00 GMT"), RETRY_AFTER, late),
            Some(Duration::from_secs(120)),
            "119.6s away is two minutes, not 119s",
        );
    }

    #[test]
    fn a_retry_after_we_cannot_read_is_not_a_reason_to_stop_forever() {
        assert_eq!(retry_after(&headers("soon"), RETRY_AFTER), None);
        assert_eq!(retry_after(&HeaderMap::new(), RETRY_AFTER), None);
        assert_eq!(
            retry_after_at(
                &headers("Tue, 22 Sep 2026 00:59:00 GMT"),
                RETRY_AFTER,
                now()
            ),
            None,
            "a date in the past is not a delay",
        );
        assert_eq!(
            retry_after_at(
                &headers("Tue, 32 Sep 2026 01:02:00 GMT"),
                RETRY_AFTER,
                now()
            ),
            None,
        );

        let past = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(retry_after(&headers(&past.to_rfc2822()), RETRY_AFTER), None);
    }
}
