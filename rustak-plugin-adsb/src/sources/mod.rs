//! Where the aircraft come from.
//!
//! Four upstreams, one shape: [`readsb`] reads a decoder's own
//! `aircraft.json`, [`aggregator`] reads the same document from a public pool
//! of receivers, [`opensky`] reads OpenSky's state vectors, and [`replay`]
//! reads a file for demonstrations and tests. Each is a
//! [`Feed`] that also answers how its upstream is
//! doing, which is what the sidecar's heartbeat reports.
//!
//! # Being a good citizen of somebody else's service
//!
//! Three of these are free services run by volunteers, and the plugin behaves
//! like it:
//!
//! - every request carries [`USER_AGENT`], which names the software and links
//!   to it, so an operator of the upstream can find out who we are;
//! - [`SourceState`] puts a floor under the request rate that is independent of
//!   the sidecar's tick, and backs off when the answer is a failure;
//! - a `429` is honoured for as long as the response asks, within reason, and a
//!   provider that asks twice inside ten polls has the poll interval raised to
//!   what it asked for until the process restarts;
//! - a provider that refuses twice inside ten polls *without* saying how long
//!   to wait — which is what adsb.lol does — is backed off from by half as much
//!   again, up to two minutes, and eased back towards after sixty polls in a
//!   row that nobody refused, so a configured `poll` is a floor and not a pin;
//! - repeated `403`s stop the source rather than hammering a service that has
//!   said no.
//!
//! # And it says so once
//!
//! `notice` is the other half of being a guest: an operator who cannot see a
//! rate limit in a log because the log is nothing but rate limits is no better
//! off than one who was never told. Every run of the same thing — an outage,
//! a rate limit, a token that will not renew — is announced once, reminded
//! about every five minutes with a count, and closed with one line.

pub mod aggregator;
mod notice;
pub mod opensky;
pub mod readsb;
pub mod replay;
mod state;

use std::time::Duration;

use rustak_client::feed::Feed;
use rustak_core::prelude::*;

pub use aggregator::{AggregatorFeed, Provider};
pub use opensky::OpenSkyFeed;
pub use readsb::ReadsbFeed;
pub use replay::ReplayFeed;
pub use state::{CLEAN_RUN, LIMIT_WINDOW, MAX_ADAPTED, MAX_BACKOFF, RECENT_POLLS, SourceState};

/// What this plugin calls itself to every upstream it reaches.
///
/// Descriptive on purpose: adsb.fi and airplanes.live both ask that a client
/// identify itself, and a service that can see who is calling can ask us to
/// stop rather than blocking an address range.
pub const USER_AGENT: &str = concat!(
    "rustak-plugin-adsb/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/SierraSoftworks/rustak)",
);

/// How long any one request to an upstream may take.
///
/// Short: a source that is wedged must not hold up the sidecar's tick, and the
/// next poll is only an interval away.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A [`Feed`] this plugin can also ask how its upstream is doing.
///
/// The shared trait is deliberately two methods wide — a name and a poll — so
/// this is the plugin's own extension rather than a change to
/// [`rustak_client::feed`]: what an administrator wants on the Services page is
/// specific to a feed that reaches out over HTTP, which not every feed does.
pub trait AdsbFeed: Feed {
    /// How the upstream is doing, for the heartbeat and for the logs.
    fn state(&self) -> &SourceState;
}

/// Builds the HTTP client a live source reaches its upstream with.
///
/// Public roots rather than a deployment truststore: these are public services
/// behind public certificates, and they have nothing to do with the rustak
/// server's own CA.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the TLS backend will not
/// initialise, which is not something an operator can do anything about.
pub fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .or_system_err(&[
            "This usually means the TLS backend could not be initialised.",
            "Please report this issue to the development team via GitHub.",
        ])
}

/// Reads the delay a `429` stated, in whichever form it stated it.
///
/// RFC 9110 §10.2.3 allows a whole number of seconds or an HTTP-date, and this
/// reads both — the date in all three of the spellings §5.6.7 obliges a
/// recipient to accept. It also reads **decimal seconds**, rounded up, which
/// the RFC does not allow and which rate limiters that think in milliseconds
/// send anyway: a provider that says `1.5` has plainly told us something, and
/// treating that as silence is how a stated delay gets ignored. `header` is
/// matched without regard to case, as header names are.
///
/// A value we cannot read, or a date in the past, answers [`None`] and the
/// caller treats the refusal as one that named no delay — being asked to wait
/// until a moment we cannot work out is not a reason to stop polling forever.
#[must_use]
pub fn retry_after(headers: &reqwest::header::HeaderMap, header: &str) -> Option<Duration> {
    retry_after_at(headers, header, chrono::Utc::now())
}

/// [`retry_after`], with an HTTP-date judged against an instant of the
/// caller's choosing.
#[must_use]
pub fn retry_after_at(
    headers: &reqwest::header::HeaderMap,
    header: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
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
/// RFC 2822 first, because that is what this accepted before and it reads a
/// numeric zone, which an HTTP-date should not carry and sometimes does. Then
/// the three forms with the day name skipped rather than checked: it says
/// nothing the date does not, and a server that gets it wrong has still told
/// us when. Month names and `GMT` are read without regard to case. A two-digit
/// year is this century's, which is all a delay can be.
fn http_date(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(at) = chrono::DateTime::parse_from_rfc2822(value) {
        return Some(at.with_timezone(&chrono::Utc));
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
    .find_map(|format| chrono::NaiveDateTime::parse_from_str(rest, format).ok())
    .map(|at| at.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn the_user_agent_names_the_software_and_links_to_it() {
        assert!(USER_AGENT.starts_with("rustak-plugin-adsb/"));
        assert!(USER_AGENT.contains("github.com/SierraSoftworks/rustak"));
    }

    /// The instant the date forms below are judged against.
    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-22T01:00:00Z")
            .expect("an instant")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn a_retry_after_in_seconds_is_read_as_seconds() {
        assert_eq!(
            retry_after(&headers("30"), "retry-after"),
            Some(Duration::from_secs(30)),
        );
        assert_eq!(
            retry_after(&headers(" 0 "), "retry-after"),
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
                retry_after(&headers(written), "retry-after"),
                Some(Duration::from_secs(expected)),
                "{written}",
            );
        }
    }

    #[test]
    fn a_number_that_is_not_a_delay_is_not_read_as_one() {
        for written in ["-1", "-0.5", "inf", "NaN", "1e400", "ten", "10s", ""] {
            assert_eq!(
                retry_after(&headers(written), "retry-after"),
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
                retry_after_at(&headers(written), "retry-after", now()),
                Some(Duration::from_secs(120)),
                "{written}",
            );
        }

        assert_eq!(
            retry_after_at(&headers("Sun Nov  1 00:00:00 2026"), "retry-after", now())
                .map(|delay| delay.as_secs() / 86_400),
            Some(39),
            "asctime pads a single-digit day with a space",
        );
    }

    #[test]
    fn neither_the_header_name_nor_the_date_is_case_sensitive() {
        let mut shouted = HeaderMap::new();
        shouted.insert(
            reqwest::header::HeaderName::from_bytes(b"Retry-After").expect("a header name"),
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
                "retry-after",
                now()
            ),
            Some(Duration::from_secs(120)),
        );
    }

    #[test]
    fn a_date_is_a_delay_in_whole_seconds_rounded_up() {
        let late = now() + chrono::Duration::milliseconds(400);

        assert_eq!(
            retry_after_at(
                &headers("Tue, 22 Sep 2026 01:02:00 GMT"),
                "retry-after",
                late
            ),
            Some(Duration::from_secs(120)),
            "119.6s away is two minutes, not 119s",
        );
    }

    #[test]
    fn a_retry_after_we_cannot_read_is_not_a_reason_to_stop_forever() {
        assert_eq!(retry_after(&headers("soon"), "retry-after"), None);
        assert_eq!(retry_after(&HeaderMap::new(), "retry-after"), None);
        assert_eq!(
            retry_after_at(
                &headers("Tue, 22 Sep 2026 00:59:00 GMT"),
                "retry-after",
                now()
            ),
            None,
            "a date in the past is not a delay",
        );
        assert_eq!(
            retry_after_at(
                &headers("Tue, 32 Sep 2026 01:02:00 GMT"),
                "retry-after",
                now()
            ),
            None,
        );

        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        assert_eq!(
            retry_after(&headers(&past.to_rfc2822()), "retry-after"),
            None
        );
    }

    #[test]
    fn the_client_builds() {
        assert!(http_client().is_ok());
    }
}
