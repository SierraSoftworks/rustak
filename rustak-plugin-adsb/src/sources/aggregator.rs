//! The public aggregators, which pool what thousands of receivers hear.
//!
//! All three serve the same `readsb` objects over an HTTP endpoint that takes a
//! point and a radius, so this is one implementation and the [`Provider`]
//! chooses the URL. The array key differs — adsb.lol and airplanes.live say
//! `ac`, adsb.fi says `aircraft` — which [`Snapshot`] absorbs, so nothing here
//! has to care.
//!
//! # These are somebody else's servers
//!
//! Every one of them is free, run by volunteers, and paid for by people who
//! feed data into it. This source is written to be a guest:
//!
//! - it identifies itself by name and link in every request;
//! - it asks no more often than [`Provider::default_poll`] unless a
//!   configuration says otherwise, and never more often than
//!   [`Provider::min_interval`] whatever a configuration says;
//! - a `429` is honoured for as long as `Retry-After` asks, and a provider that
//!   asks twice inside ten polls has the interval raised to what it asked for;
//! - [`FORBIDDEN_LIMIT`] consecutive `403`s stop the source, with a log line
//!   saying so, rather than retrying against a service that has said no.
//!
//! # The defaults came from a deployment, not from a document
//!
//! adsb.lol publishes no number — its limits are "dynamic, API keys planned" —
//! and the first live deployment found out what that meant: `429`,
//! `Retry-After: 10`, on about every other request at a five-second poll. So
//! its default is ten seconds, which is what it asked for; adsb.fi documents
//! one request a second and gets five, which is a polite margin rather than a
//! measurement; airplanes.live is unverified and gets the conservative one.
//!
//! Each provider's terms and the attribution it asks for are in the crate's
//! README, and [`Provider::terms`] carries the short version into the log at
//! start-up so that an operator sees it without reading anything.

use std::time::Duration;

use chrono::Utc;
use rustak_client::feed::{Area, Feed, Track};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{AdsbFeed, SourceState, http_client, retry_after};
use crate::mapping::track_from_aircraft;
use crate::wire::Snapshot;

/// The largest radius any of these endpoints accepts, in nautical miles.
pub const MAX_RADIUS_NM: f64 = 250.0;

/// How many consecutive `403`s stop this source for good.
///
/// Three rather than one, because a single `403` can be a proxy or a bad
/// minute; three in a row is a service telling us to go away, and continuing to
/// ask would be the thing that gets an address range blocked.
pub const FORBIDDEN_LIMIT: u32 = 3;

/// The fastest any of these services is asked, whatever a configuration says.
///
/// Two seconds rather than the one they document: the data behind all three is
/// a pool of receivers that updates about once a second, so a second request
/// inside two seconds costs somebody else's bandwidth to hear almost the same
/// aircraft twice. A deployment that needs more than this wants a receiver of
/// its own, which the `readsb` source reads for nothing.
pub const POLL_FLOOR: Duration = Duration::from_secs(2);

/// Which pool of receivers to read.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// `adsb.lol` — open data, no key, rate limits described as dynamic.
    AdsbLol,
    /// `adsb.fi` — open data, non-commercial use, asks to be cited and linked.
    AdsbFi,
    /// `airplanes.live` — documented as the same shape at one request a
    /// second. **Experimental**: our probe of the public endpoint answered
    /// `403`, so this path is implemented but unverified.
    AirplanesLive,
}

impl Provider {
    /// What to call it in a log line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AdsbLol => "adsb.lol",
            Self::AdsbFi => "adsb.fi",
            Self::AirplanesLive => "airplanes.live",
        }
    }

    /// The endpoint for a circle, as the provider spells it.
    #[must_use]
    pub fn endpoint(self, lat: f64, lon: f64, radius_nm: u32) -> String {
        match self {
            Self::AdsbLol => format!("https://api.adsb.lol/v2/point/{lat:.5}/{lon:.5}/{radius_nm}"),
            Self::AdsbFi => format!(
                "https://opendata.adsb.fi/api/v2/lat/{lat:.5}/lon/{lon:.5}/dist/{radius_nm}"
            ),
            Self::AirplanesLive => {
                format!("https://api.airplanes.live/v2/point/{lat:.5}/{lon:.5}/{radius_nm}")
            }
        }
    }

    /// How often this provider is asked when a configuration does not say.
    ///
    /// Not a published figure in any of the three cases: adsb.lol's is what it
    /// asked the first live deployment for in a `Retry-After`, and the other
    /// two are a margin under what they document, because being refused is
    /// worse for a feed than being a little behind.
    #[must_use]
    pub const fn default_poll(self) -> Duration {
        match self {
            // Observed: `429`, `Retry-After: 10`, on about every other request
            // at five seconds.
            Self::AdsbLol => Duration::from_secs(10),
            // Documented at one request a second; five is the polite margin.
            Self::AdsbFi => Duration::from_secs(5),
            // Unverified — our probe of the endpoint was refused — so the
            // conservative one until somebody has run it.
            Self::AirplanesLive => Duration::from_secs(10),
        }
    }

    /// The shortest gap between two requests this source will ever leave.
    ///
    /// [`POLL_FLOOR`] for all three today. It is a method rather than a
    /// constant read directly because these are three separate services, and
    /// the first one to publish a number of its own belongs here.
    #[must_use]
    pub const fn min_interval(self) -> Duration {
        POLL_FLOOR
    }

    /// The one-line version of what using this data commits an operator to.
    #[must_use]
    pub const fn terms(self) -> &'static str {
        match self {
            Self::AdsbLol => "adsb.lol is open data; see https://adsb.lol for the current terms.",
            Self::AdsbFi => {
                "adsb.fi is for non-commercial use and asks to be cited and linked: https://adsb.fi"
            }
            Self::AirplanesLive => {
                "airplanes.live documents a ceiling of one request a second; \
                 see https://airplanes.live/api-guide"
            }
        }
    }
}

/// One public aggregator, read over a circle.
#[derive(Debug)]
pub struct AggregatorFeed {
    provider: Provider,
    url: reqwest::Url,
    client: reqwest::Client,
    state: SourceState,
    forbidden: u32,
    stopped: bool,
}

impl AggregatorFeed {
    /// Points a provider at the area this sidecar is watching.
    ///
    /// The area becomes a centre and a radius, because that is what these
    /// endpoints take; a box therefore asks for the circle that encloses it and
    /// the publisher filters the corners back out.
    ///
    /// `poll` is what the configuration asked for, or [`None`] for the
    /// provider's own [`default_poll`](Provider::default_poll). Either way it
    /// is clamped up to [`min_interval`](Provider::min_interval), and an
    /// operator who asked for something faster is told which one they got.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the HTTP client or the URL
    /// this crate built cannot be constructed, neither of which an operator can
    /// do anything about.
    pub fn open(provider: Provider, area: Area, poll: Option<Duration>) -> Result<Self, Error> {
        let floor = provider.min_interval();
        let asked = poll.unwrap_or_else(|| provider.default_poll());
        let interval = asked.max(floor);

        if interval > asked {
            info!(
                provider = provider.name(),
                asked_s = asked.as_secs(),
                seconds = floor.as_secs(),
                "The configured poll interval is faster than this source will ask a free service; \
                 using {}s.",
                floor.as_secs(),
            );
        }

        let (lat, lon) = area.centre();
        let radius = radius_nm(area);
        let endpoint = provider.endpoint(lat, lon, radius);
        let url =
            reqwest::Url::parse(&endpoint).or_system_err(rustak_core::errors::ADVICE_REPORT_DEV)?;

        info!(
            provider = provider.name(),
            lat,
            lon,
            radius_nm = radius,
            poll_s = interval.as_secs(),
            "Reading aircraft from a public aggregator. {}",
            provider.terms(),
        );

        Ok(Self {
            provider,
            url,
            client: http_client()?,
            state: SourceState::new(provider.name(), interval),
            forbidden: 0,
            stopped: false,
        })
    }

    /// Whether repeated refusals have stopped this source.
    #[must_use]
    pub const fn stopped(&self) -> bool {
        self.stopped
    }

    /// One request, turned into aircraft or into a reason there are none.
    async fn fetch(&mut self) -> Result<Vec<Track>, Error> {
        let response = self
            .client
            .get(self.url.clone())
            .send()
            .await
            .wrap_user_err(
                format!("We could not reach {}.", self.provider.name()),
                &[
                    "Check that this machine can reach the internet.",
                    "A public aggregator is somebody else's service; it may simply be down.",
                ],
            )?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // The header, when there is one, is the provider saying how often
            // it wants to be asked; the state machine is what remembers that.
            self.state
                .wait_for(retry_after(response.headers(), "retry-after"));

            return Ok(Vec::new());
        }

        if status == reqwest::StatusCode::FORBIDDEN {
            self.refused();

            return Ok(Vec::new());
        }

        let body = response
            .error_for_status()
            .wrap_user_err(
                format!("{} answered {status}.", self.provider.name()),
                &["A public aggregator's terms and endpoints change; check the crate's README."],
            )?
            .text()
            .await
            .wrap_user_err(
                format!("{} sent a broken reply.", self.provider.name()),
                rustak_core::errors::ADVICE_RESTART_AFTER_FIXING,
            )?;

        let snapshot: Snapshot = serde_json::from_str(&body).wrap_user_err(
            format!(
                "{} sent something we could not read as aircraft.",
                self.provider.name()
            ),
            &["The endpoint may have changed shape; check the crate's README."],
        )?;

        self.forbidden = 0;
        self.state.succeeded();

        let now = Utc::now();

        Ok(snapshot
            .into_aircraft()
            .iter()
            .filter_map(|aircraft| track_from_aircraft(aircraft, now))
            .collect())
    }

    /// Counts a refusal, and stops asking once there have been enough.
    fn refused(&mut self) {
        self.forbidden = self.forbidden.saturating_add(1);
        self.state.failed(format!(
            "{} refused the request (403).",
            self.provider.name()
        ));

        if self.forbidden >= FORBIDDEN_LIMIT {
            self.stopped = true;

            error!(
                provider = self.provider.name(),
                refusals = self.forbidden,
                "This aggregator has refused every request; the source has stopped. {}",
                self.provider.terms(),
            );
        }
    }
}

/// The radius to ask for, in whole nautical miles within what the endpoints
/// accept.
fn radius_nm(area: Area) -> u32 {
    let clamped = area.radius_nm().ceil().clamp(1.0, MAX_RADIUS_NM);

    // The clamp above puts this between 1 and 250, so the conversion cannot
    // fail; the fallback is the maximum rather than a panic.
    u32::try_from(clamped as i64).unwrap_or(MAX_RADIUS_NM as u32)
}

#[async_trait]
impl Feed for AggregatorFeed {
    fn name(&self) -> &str {
        self.provider.name()
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        if self.stopped || !self.state.ready() {
            return Ok(Vec::new());
        }

        match self.fetch().await {
            Ok(tracks) => Ok(tracks),
            Err(err) => {
                self.state.failed(err.to_string());

                Err(err)
            }
        }
    }
}

impl AdsbFeed for AggregatorFeed {
    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The aggregator pointed at a mock rather than at somebody's live service.
    fn against(server: &MockServer, interval: Duration) -> AggregatorFeed {
        let mut feed = AggregatorFeed::open(Provider::AdsbLol, Area::default(), Some(interval))
            .expect("it opens");
        feed.url = reqwest::Url::parse(&format!("{}/v2/point/51.5/-0.5/25", server.uri()))
            .expect("a mock URL");
        // `open` clamps the interval up to the floor under a free service,
        // which is right against a live one and pointless against a mock.
        feed.state = SourceState::new(Provider::AdsbLol.name(), interval);

        feed
    }

    async fn serving(status: u16, body: &str) -> MockServer {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;

        server
    }

    #[test]
    fn each_provider_has_its_own_endpoint_shape() {
        assert_eq!(
            Provider::AdsbLol.endpoint(51.4775, -0.4614, 25),
            "https://api.adsb.lol/v2/point/51.47750/-0.46140/25",
        );
        assert_eq!(
            Provider::AdsbFi.endpoint(51.4775, -0.4614, 25),
            "https://opendata.adsb.fi/api/v2/lat/51.47750/lon/-0.46140/dist/25",
        );
        assert_eq!(
            Provider::AirplanesLive.endpoint(51.4775, -0.4614, 25),
            "https://api.airplanes.live/v2/point/51.47750/-0.46140/25",
        );
    }

    #[test]
    fn a_provider_is_named_in_a_settings_file_in_snake_case() {
        for (written, expected) in [
            ("\"adsb_lol\"", Provider::AdsbLol),
            ("\"adsb_fi\"", Provider::AdsbFi),
            ("\"airplanes_live\"", Provider::AirplanesLive),
        ] {
            assert_eq!(
                serde_json::from_str::<Provider>(written).expect("it parses"),
                expected,
            );
        }
    }

    #[test]
    fn the_radius_stays_inside_what_the_endpoints_accept() {
        assert_eq!(
            radius_nm(Area::Circle {
                lat: 51.0,
                lon: 0.0,
                radius_km: 46.3
            }),
            25,
        );
        assert_eq!(
            radius_nm(Area::Circle {
                lat: 51.0,
                lon: 0.0,
                radius_km: 0.1
            }),
            1,
            "a tiny circle still has to ask for something",
        );
        assert_eq!(
            radius_nm(Area::default()),
            250,
            "the whole world asks for the largest circle they will serve",
        );
    }

    #[tokio::test]
    async fn a_response_under_the_ac_key_becomes_tracks() {
        let server = serving(
            200,
            r#"{"now":1.0,"total":1,"ac":[{"hex":"3c6444","flight":"BAW117  ","category":"A5","lat":51.46,"lon":-0.39,"gs":247.6,"track":88.0,"alt_geom":3225,"seen_pos":0.1}]}"#,
        )
        .await;
        let mut feed = against(&server, Duration::from_secs(0));

        let tracks = feed.poll().await.expect("the aggregator answers");

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "ADSB-3c6444");
        assert_eq!(tracks[0].callsign.as_deref(), Some("BAW117"));
        assert!(feed.state().is_connected());
    }

    #[tokio::test]
    async fn a_response_under_the_aircraft_key_becomes_the_same_tracks() {
        let server = serving(
            200,
            r#"{"now":1.0,"resultCount":1,"aircraft":[{"hex":"3c6444","lat":51.46,"lon":-0.39,"seen_pos":0.1}]}"#,
        )
        .await;
        let mut feed = against(&server, Duration::from_secs(0));

        assert_eq!(feed.poll().await.expect("it answers")[0].id, "ADSB-3c6444");
    }

    #[tokio::test]
    async fn a_rate_limit_is_waited_out_rather_than_treated_as_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
            .mount(&server)
            .await;

        let mut feed = against(&server, Duration::from_secs(0));
        feed.state.succeeded();

        assert!(feed.poll().await.expect("429 is not an error").is_empty());
        assert!(
            feed.state().is_connected(),
            "they answered; they just said later"
        );
        assert!(!feed.state().ready(), "and we are waiting");
        assert_eq!(feed.state().rate_limited(), 1);
    }

    #[tokio::test]
    async fn a_provider_that_asks_twice_for_ten_seconds_gets_ten_seconds() {
        // The Dublin finding, over the wire: the `Retry-After` header is read,
        // and the second one inside the window raises the cadence for good.
        //
        // `fetch` rather than `poll`, because `poll` honours the wait the first
        // 429 just asked for — which is the behaviour under test everywhere
        // else and would make this suite sleep for ten seconds.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "10"))
            .mount(&server)
            .await;

        let mut feed = against(&server, Duration::from_secs(5));

        assert!(feed.fetch().await.expect("429 is not an error").is_empty());
        assert_eq!(
            feed.state().interval(),
            Duration::from_secs(5),
            "one is a bad minute",
        );

        assert!(feed.fetch().await.expect("429 is not an error").is_empty());

        assert_eq!(
            feed.state().interval(),
            Duration::from_secs(10),
            "two inside the window is adsb.lol saying how often it wants to be asked",
        );
        assert_eq!(feed.state().rate_limited(), 2);
    }

    #[test]
    fn each_provider_has_a_default_poll_and_none_of_them_is_faster_than_the_floor() {
        assert_eq!(Provider::AdsbLol.default_poll(), Duration::from_secs(10));
        assert_eq!(Provider::AdsbFi.default_poll(), Duration::from_secs(5));
        assert_eq!(
            Provider::AirplanesLive.default_poll(),
            Duration::from_secs(10),
        );

        for provider in [Provider::AdsbLol, Provider::AdsbFi, Provider::AirplanesLive] {
            assert!(
                provider.default_poll() >= provider.min_interval(),
                "{}",
                provider.name(),
            );
        }
    }

    #[test]
    fn a_poll_interval_faster_than_the_floor_is_clamped_up_to_it() {
        let feed = AggregatorFeed::open(
            Provider::AdsbLol,
            Area::default(),
            Some(Duration::from_secs(1)),
        )
        .expect("it opens");

        assert_eq!(feed.state().interval(), POLL_FLOOR);
    }

    #[test]
    fn a_source_that_names_no_poll_gets_the_provider_its_own() {
        for (provider, expected) in [
            (Provider::AdsbLol, Duration::from_secs(10)),
            (Provider::AdsbFi, Duration::from_secs(5)),
        ] {
            let feed = AggregatorFeed::open(provider, Area::default(), None).expect("it opens");

            assert_eq!(feed.state().interval(), expected, "{}", provider.name());
        }
    }

    #[test]
    fn a_poll_interval_slower_than_the_floor_is_the_one_that_is_used() {
        let feed = AggregatorFeed::open(
            Provider::AdsbLol,
            Area::default(),
            Some(Duration::from_secs(30)),
        )
        .expect("it opens");

        assert_eq!(
            feed.state().interval(),
            Duration::from_secs(30),
            "an explicit poll wins; the floor is a floor, not a schedule",
        );
    }

    #[tokio::test]
    async fn repeated_refusals_stop_the_source_for_good() {
        let server = serving(403, "no").await;
        let mut feed = against(&server, Duration::from_secs(0));

        for _ in 0..FORBIDDEN_LIMIT {
            assert!(feed.poll().await.expect("a 403 is not an error").is_empty());
        }

        assert!(
            feed.stopped(),
            "three refusals in a row is a service saying no"
        );

        let before = server.received_requests().await.expect("a log").len();
        assert!(feed.poll().await.expect("a stopped source").is_empty());
        assert_eq!(
            server.received_requests().await.expect("a log").len(),
            before,
            "a stopped source sends nothing at all",
        );
    }

    #[tokio::test]
    async fn one_refusal_is_a_bad_minute_rather_than_a_ban() {
        let server = serving(403, "no").await;
        let mut feed = against(&server, Duration::from_secs(0));

        assert!(feed.poll().await.expect("it answers nothing").is_empty());

        assert!(!feed.stopped());
    }

    #[tokio::test]
    async fn the_interval_is_a_floor_under_the_request_rate() {
        let server = serving(200, r#"{"now":1.0,"ac":[]}"#).await;
        let mut feed = against(&server, Duration::from_secs(60));

        for _ in 0..5 {
            assert!(feed.poll().await.expect("a poll").is_empty());
        }

        assert_eq!(
            server.received_requests().await.expect("a log").len(),
            1,
            "five polls inside one interval is one request",
        );
    }

    #[tokio::test]
    async fn a_server_error_is_a_failure_the_plugin_can_log() {
        let server = serving(500, "oops").await;
        let mut feed = against(&server, Duration::from_secs(0));

        let err = feed.poll().await.expect_err("a 500 is a failed poll");

        assert!(err.to_string().contains("adsb.lol"), "{err}");
        assert!(!feed.state().is_connected());
    }
}
