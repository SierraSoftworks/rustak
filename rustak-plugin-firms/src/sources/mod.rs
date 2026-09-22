//! Where the detections come from.
//!
//! Two upstreams, one shape: [`firms`] reads NASA FIRMS' area API, and
//! [`replay`] reads a CSV file in the same format for demonstrations and
//! tests. Each is a [`HotspotFeed`]: it owns its own schedule and backoff, and
//! answers how its upstream is doing so the heartbeat can say more than
//! "healthy".
//!
//! [`rustak_client::feed::Feed`] is not reused because it is a feed of
//! `Track`s, and a detection is not one: see [`crate::hotspots`].

pub mod firms;
pub mod replay;
mod state;

use std::time::Duration;

use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

pub use firms::{FirmsFeed, Sensor};
pub use replay::ReplayFeed;
pub use state::{MAX_BACKOFF, SourceState};

use crate::wire::Detection;

/// What this plugin calls itself to FIRMS, so that its operators can find out
/// who is calling and ask us to stop rather than blocking an address range.
pub const USER_AGENT: &str = concat!(
    "rustak-plugin-firms/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/SierraSoftworks/rustak)",
);

/// How long any one request may take. Longer than the other feeds allow,
/// because a continent's worth of detections is megabytes of CSV; still
/// bounded, because a wedged request must not hold up the sidecar's tick.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The most of one reply that is held in memory.
///
/// `max_detections` bounds what is kept, but only after a reply has been read;
/// this bounds the reading. Five days of every VIIRS detection on Earth is
/// tens of megabytes, so the ceiling is above any honest request and below what
/// would trouble the micro-server this is meant to run on.
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// A source of detections that also says how its upstream is doing.
#[async_trait]
pub trait HotspotFeed: Send {
    /// What to call this feed in logs and health reports.
    fn name(&self) -> &str;

    /// Everything the upstream currently reports, or nothing when it is not
    /// yet time to ask again. Returning the same detection on every poll is
    /// expected: [`crate::hotspots::Hotspots`] knows what it has seen.
    ///
    /// # Errors
    ///
    /// Whatever went wrong, already recorded in [`state`](Self::state). An
    /// error is an outage to wait out, never a reason to stop the sidecar.
    async fn poll(&mut self) -> Result<Vec<Detection>, Error>;

    /// How the upstream is doing, for the heartbeat and for the logs.
    fn state(&self) -> &SourceState;
}

/// Builds the HTTP client the live source reaches FIRMS with. Public roots:
/// FIRMS is a public service and has nothing to do with the rustak CA.
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

/// Reads a reply's body as text, refusing one larger than `limit` bytes
/// instead of buffering it: by its declared length when it has one, and by
/// what actually arrives when it does not or when that was a lie.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the reply is too large, which an
/// operator fixes with a smaller request, or when the connection broke. Neither
/// carries the request URL.
pub async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<String, Error> {
    let too_large = || {
        human_errors::user(
            format!(
                "NASA FIRMS sent more than {limit} bytes for one request, which is more than this sidecar will hold."
            ),
            &["Narrow `[settings.area]`, or lower `[settings.source] days`."],
        )
    };

    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(too_large());
    }

    let mut body: Vec<u8> = Vec::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)
        .wrap_user_err(
            "NASA FIRMS sent a broken reply.",
            &["FIRMS is somebody else's service; the next poll may simply work."],
        )?
    {
        if body.len() + chunk.len() > limit {
            return Err(too_large());
        }

        body.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Reads a `Retry-After` header, which is a number of seconds or an HTTP date.
/// One we cannot read answers [`None`], and the caller falls back to its own
/// guess rather than waiting for a moment it cannot work out.
#[must_use]
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .to_string();

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let at = chrono::DateTime::parse_from_rfc2822(&value).ok()?;

    (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
        .to_std()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn the_user_agent_names_the_software_and_links_to_it() {
        assert!(USER_AGENT.starts_with("rustak-plugin-firms/"));
        assert!(USER_AGENT.contains("github.com/SierraSoftworks/rustak"));
    }

    #[tokio::test]
    async fn a_reply_is_read_up_to_a_ceiling_and_no_further() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(1_000)))
            .mount(&server)
            .await;

        let get = || async {
            http_client()
                .expect("a client")
                .get(server.uri())
                .send()
                .await
                .expect("the mock answers")
        };

        assert_eq!(
            read_bounded(get().await, 10_000)
                .await
                .expect("it fits")
                .len(),
            1_000,
        );

        let err = read_bounded(get().await, 100)
            .await
            .expect_err("ten times the ceiling");

        assert!(err.to_string().contains("100 bytes"), "{err}");
    }

    #[test]
    fn a_retry_after_is_read_in_either_form_or_not_at_all() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_str(value).unwrap());
            headers
        };
        let soon = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc2822();

        assert_eq!(retry_after(&header("30")), Some(Duration::from_secs(30)));
        assert!(retry_after(&header(&soon)).is_some_and(|d| d > Duration::from_secs(100)));
        assert_eq!(retry_after(&header("soon")), None);
        assert_eq!(retry_after(&HeaderMap::new()), None);
    }
}
