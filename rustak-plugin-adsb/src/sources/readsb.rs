//! A `readsb`/`dump1090` receiver's own `aircraft.json`.
//!
//! The best ADS-B source there is, and the only one with no terms attached: a
//! receiver on a windowsill decoding 1090 MHz, writing what it hears to
//! `/run/readsb/aircraft.json` about once a second. `tar1090` and the
//! `dump1090` web interfaces serve the same file over HTTP at
//! `…/data/aircraft.json`, so one setting covers both: a path is read from
//! disk, an `http://` or `https://` URL is fetched.
//!
//! There is no rate limit to respect here — it is the operator's own hardware —
//! but the poll interval still exists, because re-reading a file that is
//! rewritten once a second ten times a second is a waste of both ends.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use rustak_client::feed::{Feed, Track};
use rustak_client::sidecar::async_trait;
use rustak_core::errors::ADVICE_FILE_ACCESS;
use rustak_core::prelude::*;

use super::{AdsbFeed, SourceState, http_client};
use crate::mapping::track_from_aircraft;
use crate::wire::Snapshot;

/// Advice for a receiver that is not answering.
const ADVICE_RECEIVER: &[&str] = &[
    "Check that the receiver is running and that `aircraft.json` is where [settings.source] url_or_path says.",
    "A tar1090 or dump1090 web interface serves it at <base>/data/aircraft.json.",
];

/// A local or networked `readsb` decoder.
#[derive(Debug)]
pub struct ReadsbFeed {
    location: Location,
    client: Option<reqwest::Client>,
    state: SourceState,
}

/// Where the document is.
#[derive(Clone, Debug)]
enum Location {
    /// A file on this machine, which is the local-receiver case.
    File(PathBuf),
    /// An HTTP(S) URL, which is the tar1090 case.
    Http(reqwest::Url),
}

impl ReadsbFeed {
    /// Opens a receiver, by path or by URL.
    ///
    /// Nothing is read here: a receiver that is down at start-up is an outage
    /// rather than a configuration error, and a sidecar that refused to start
    /// over one would need restarting by hand when it came back.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the setting looks like a URL
    /// and will not parse as one, and a [`human_errors::Kind::System`] error
    /// when the HTTP client cannot be built.
    pub fn open(url_or_path: &str, interval: Duration) -> Result<Self, Error> {
        let trimmed = url_or_path.trim();
        let is_url = trimmed.starts_with("http://") || trimmed.starts_with("https://");

        let (location, client) = if is_url {
            let url = reqwest::Url::parse(trimmed).wrap_user_err(
                format!("We could not read '{trimmed}' as a URL."),
                &[
                    "A readsb source is either a file path or an http:// or https:// URL.",
                    "A tar1090 receiver serves the document at <base>/data/aircraft.json.",
                ],
            )?;

            (Location::Http(url), Some(http_client()?))
        } else {
            (Location::File(PathBuf::from(trimmed)), None)
        };

        Ok(Self {
            state: SourceState::new(location.describe(), interval),
            location,
            client,
        })
    }

    /// Fetches the document, from wherever it is.
    async fn fetch(&self) -> Result<String, Error> {
        match (&self.location, &self.client) {
            (Location::File(path), _) => tokio::fs::read_to_string(path).await.wrap_user_err(
                format!(
                    "We could not read the receiver's file '{}'.",
                    path.display()
                ),
                ADVICE_FILE_ACCESS,
            ),
            (Location::Http(url), Some(client)) => {
                let response = client
                    .get(url.clone())
                    .send()
                    .await
                    .wrap_user_err(
                        format!(
                            "We could not reach the receiver at '{}'.",
                            self.state.name()
                        ),
                        ADVICE_RECEIVER,
                    )?
                    .error_for_status()
                    .wrap_user_err(
                        format!(
                            "The receiver at '{}' refused the request.",
                            self.state.name()
                        ),
                        ADVICE_RECEIVER,
                    )?;

                response.text().await.wrap_user_err(
                    format!(
                        "The receiver at '{}' sent a broken reply.",
                        self.state.name()
                    ),
                    ADVICE_RECEIVER,
                )
            }
            // Unreachable: a `Location::Http` is only ever built beside a
            // client. Written out rather than unwrapped so that a future
            // variant cannot make it a panic.
            (Location::Http(_), None) => Err(human_errors::system(
                "This ADS-B source has a URL but no HTTP client.",
                rustak_core::errors::ADVICE_REPORT_DEV,
            )),
        }
    }
}

impl Location {
    /// What to call this receiver in a log line, with any user information in a
    /// URL left out of it.
    fn describe(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Http(url) => match url.host_str() {
                Some(host) => format!("{}://{host}{}", url.scheme(), url.path()),
                None => url.path().to_string(),
            },
        }
    }
}

#[async_trait]
impl Feed for ReadsbFeed {
    fn name(&self) -> &str {
        self.state.name()
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        if !self.state.ready() {
            return Ok(Vec::new());
        }

        let body = match self.fetch().await {
            Ok(body) => body,
            Err(err) => {
                self.state.failed(err.to_string());

                return Err(err);
            }
        };

        let snapshot: Snapshot = match serde_json::from_str(&body) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let err = human_errors::user(
                    format!(
                        "The receiver at '{}' sent something that is not an aircraft.json ({err}).",
                        self.state.name(),
                    ),
                    ADVICE_RECEIVER,
                );
                self.state.failed(err.to_string());

                return Err(err);
            }
        };

        self.state.succeeded();

        let now = Utc::now();

        Ok(snapshot
            .into_aircraft()
            .iter()
            .filter_map(|aircraft| track_from_aircraft(aircraft, now))
            .collect())
    }
}

impl AdsbFeed for ReadsbFeed {
    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE: &str = include_str!("../../tests/fixtures/readsb.json");

    async fn receiver(body: &str) -> MockServer {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/data/aircraft.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        server
    }

    #[tokio::test]
    async fn a_file_on_disk_is_read_and_mapped() {
        let directory = tempfile::tempdir().expect("a directory");
        let file = directory.path().join("aircraft.json");
        std::fs::write(&file, FIXTURE).expect("the fixture lands");

        let mut feed = ReadsbFeed::open(&file.display().to_string(), Duration::from_secs(0))
            .expect("it opens");

        let tracks = feed.poll().await.expect("the file is read");

        assert_eq!(tracks.len(), 5, "two of the seven have no usable position");
        assert!(feed.state().is_connected());
        assert!(feed.name().ends_with("aircraft.json"));
    }

    #[tokio::test]
    async fn a_tar1090_url_is_fetched_and_mapped() {
        let server = receiver(FIXTURE).await;
        let mut feed = ReadsbFeed::open(
            &format!("{}/data/aircraft.json", server.uri()),
            Duration::from_secs(0),
        )
        .expect("it opens");

        let tracks = feed.poll().await.expect("the receiver answers");

        assert_eq!(tracks.len(), 5);
        assert_eq!(tracks[0].id, "ADSB-3c6444");
        assert!(feed.state().is_connected());
    }

    #[tokio::test]
    async fn every_request_names_this_plugin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::header(
                "user-agent",
                super::super::USER_AGENT,
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let mut feed = ReadsbFeed::open(
            &format!("{}/data/aircraft.json", server.uri()),
            Duration::from_secs(0),
        )
        .expect("it opens");

        assert_eq!(feed.poll().await.expect("a matching request").len(), 5);
    }

    #[tokio::test]
    async fn the_interval_is_a_floor_under_the_request_rate() {
        // The sidecar ticks far more often than an upstream wants to be asked;
        // this is the difference between a feed and a denial of service.
        let server = receiver(FIXTURE).await;
        let mut feed = ReadsbFeed::open(
            &format!("{}/data/aircraft.json", server.uri()),
            Duration::from_secs(60),
        )
        .expect("it opens");

        assert_eq!(feed.poll().await.expect("the first poll").len(), 5);

        for _ in 0..5 {
            assert!(
                feed.poll().await.expect("a throttled poll").is_empty(),
                "a poll inside the interval reaches nothing and answers nothing",
            );
        }

        assert_eq!(
            server
                .received_requests()
                .await
                .expect("a request log")
                .len(),
            1,
            "six polls, one request",
        );
    }

    #[tokio::test]
    async fn a_receiver_that_is_down_is_an_error_rather_than_a_panic() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let mut feed = ReadsbFeed::open(
            &format!("{}/data/aircraft.json", server.uri()),
            Duration::from_secs(0),
        )
        .expect("it opens");

        let err = feed.poll().await.expect_err("a 503 is a failed poll");

        assert!(err.to_string().contains("refused"), "{err}");
        assert!(!feed.state().is_connected());
        assert!(!feed.state().ever_connected());
        assert!(feed.state().last_error().is_some());
    }

    #[tokio::test]
    async fn a_document_that_is_not_an_aircraft_json_says_so() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>not json</html>"))
            .mount(&server)
            .await;

        let mut feed = ReadsbFeed::open(
            &format!("{}/data/aircraft.json", server.uri()),
            Duration::from_secs(0),
        )
        .expect("it opens");

        let err = feed.poll().await.expect_err("html is not a snapshot");

        assert!(err.to_string().contains("aircraft.json"), "{err}");
    }

    #[test]
    fn a_url_with_user_information_is_not_repeated_into_the_logs() {
        let feed = ReadsbFeed::open(
            "http://operator:hunter2@receiver.lan/data/aircraft.json",
            Duration::from_secs(1),
        )
        .expect("it opens");

        assert_eq!(feed.name(), "http://receiver.lan/data/aircraft.json");
        assert!(!feed.name().contains("hunter2"));
    }

    #[test]
    fn a_url_that_will_not_parse_is_refused_at_start_up() {
        let err =
            ReadsbFeed::open("http://", Duration::from_secs(1)).expect_err("that is not a URL");

        assert!(err.to_string().contains("URL"), "{err}");
    }
}
