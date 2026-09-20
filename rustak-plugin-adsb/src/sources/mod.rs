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
//! - a `429` is honoured for as long as the response asks, within reason;
//! - repeated `403`s stop the source rather than hammering a service that has
//!   said no.

pub mod aggregator;
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
pub use state::{MAX_BACKOFF, SourceState};

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

/// Reads a `Retry-After` header, which is a number of seconds or an HTTP date.
///
/// A date we cannot parse, or one in the past, answers [`None`] and the caller
/// falls back to its own backoff — being asked to wait until a moment we cannot
/// work out is not a reason to stop polling forever.
#[must_use]
pub fn retry_after(headers: &reqwest::header::HeaderMap, header: &str) -> Option<Duration> {
    let value = headers.get(header)?.to_str().ok()?.trim().to_string();

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let at = chrono::DateTime::parse_from_rfc2822(&value).ok()?;
    let delay = at.with_timezone(&chrono::Utc) - chrono::Utc::now();

    delay.to_std().ok()
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

    #[test]
    fn a_retry_after_in_seconds_is_read_as_seconds() {
        assert_eq!(
            retry_after(&headers("30"), "retry-after"),
            Some(Duration::from_secs(30)),
        );
    }

    #[test]
    fn a_retry_after_as_a_date_is_read_as_a_delay() {
        let at = chrono::Utc::now() + chrono::Duration::seconds(120);
        let delay = retry_after(&headers(&at.to_rfc2822()), "retry-after")
            .expect("an HTTP date is a delay");

        assert!(delay > Duration::from_secs(100), "{delay:?}");
        assert!(delay <= Duration::from_secs(120), "{delay:?}");
    }

    #[test]
    fn a_retry_after_we_cannot_read_is_not_a_reason_to_stop_forever() {
        assert_eq!(retry_after(&headers("soon"), "retry-after"), None);
        assert_eq!(retry_after(&HeaderMap::new(), "retry-after"), None);

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
