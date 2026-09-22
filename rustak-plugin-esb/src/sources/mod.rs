//! Where the outages come from.
//!
//! [`powercheck`] reads ESB Networks' PowerCheck API and [`replay`] reads a
//! file, for demonstrations and tests. Either is an [`OutageFeed`]: it owns its
//! own cadence, backoff and memory, answers **everything currently listed** on
//! every poll, and says how its upstream is doing for the heartbeat.

pub mod powercheck;
pub mod replay;
mod state;

use std::time::Duration;

use reqwest::header::HeaderMap;
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use crate::outage::Outage;

pub use powercheck::PowerCheckFeed;
pub use replay::ReplayFeed;
pub use state::{MAX_BACKOFF, MAX_RETRY_AFTER, SourceState};

/// What this plugin calls itself upstream, so that ESB can tell who is asking.
pub const USER_AGENT: &str = concat!(
    "rustak-plugin-esb/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/SierraSoftworks/rustak)",
);

/// How long any one request may take; the next tick is only seconds away.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A source of outages.
#[async_trait]
pub trait OutageFeed: Send {
    /// The upstream's name, for a log line.
    fn name(&self) -> &str;

    /// Every outage currently listed, as far as this source knows.
    ///
    /// Called on every tick, which is far more often than an upstream is
    /// asked: a poll between requests answers what the last one learned.
    ///
    /// # Errors
    ///
    /// Whatever went wrong upstream, for the plugin to log. Never a reason to
    /// stop the sidecar, and never a reason to clear the map.
    async fn poll(&mut self) -> Result<Vec<Outage>, Error>;

    /// How the upstream is doing.
    fn state(&self) -> &SourceState;
}

/// Builds the HTTP client a live source uses, against the public roots.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the TLS backend will not start.
pub fn http_client(headers: HeaderMap) -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .default_headers(headers)
        .build()
        .or_system_err(&[
            "This usually means the TLS backend could not be initialised.",
            "Please report this issue to the development team via GitHub.",
        ])
}

/// Reads a `Retry-After` header: a number of seconds, or an HTTP date.
#[must_use]
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;

    (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
        .to_std()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;
    use rstest::rstest;

    #[rstest]
    #[case("30", Some(Duration::from_secs(30)))]
    #[case("soon", None)]
    fn a_retry_after_is_a_delay_or_nothing(
        #[case] value: &str,
        #[case] expected: Option<Duration>,
    ) {
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            HeaderValue::from_str(value).expect("a header"),
        );

        assert_eq!(retry_after(&headers), expected);
        assert_eq!(retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn a_retry_after_as_a_date_is_the_delay_until_then() {
        let at = chrono::Utc::now() + chrono::Duration::seconds(120);
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            HeaderValue::from_str(&at.to_rfc2822()).expect("a header"),
        );

        let delay = retry_after(&headers).expect("an HTTP date is a delay");

        assert!(
            delay > Duration::from_secs(100) && delay <= Duration::from_secs(120),
            "{delay:?}"
        );
    }

    #[test]
    fn the_user_agent_names_the_software_and_links_to_it() {
        assert!(USER_AGENT.starts_with("rustak-plugin-esb/"));
        assert!(USER_AGENT.contains("github.com/SierraSoftworks/rustak"));
    }
}
