//! The OpenSky Network's state vectors.
//!
//! A research network rather than a hobbyist pool: `GET /api/states/all` over a
//! bounding box, anonymously at 10-second resolution or with an OAuth2 client
//! credential at 5. It is the only source here that needs a credential, and the
//! only one whose terms are explicitly **research and non-commercial** — see
//! the crate's README before pointing a deployment at it.
//!
//! # Credits
//!
//! OpenSky charges a daily budget rather than a rate limit: 400 credits a day
//! anonymously, 4000 authenticated, and one request costs 1 credit for a box of
//! up to 25 square degrees, 2 up to 100, 3 up to 400 and 4 above that. A
//! 1°×1° box polled every 10 seconds is 8640 requests a day and will run out
//! before lunch, so [`OpenSkyFeed::open`] says what a configured area costs at
//! start-up and warns when it costs more than one credit.
//!
//! # The token
//!
//! Client credentials, exchanged for a bearer token; the `token` module is
//! that half, including how loudly a token endpoint that is down may complain.

mod token;

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_client::feed::{Area, Feed, Track};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{AdsbFeed, SourceState, http_client, retry_after};
use crate::mapping::track_from_state;
use crate::wire::OpenSkyStates;
use token::{ADVICE_CREDENTIAL, Tokens};

/// Where the state vectors are.
pub const STATES_URL: &str = "https://opensky-network.org/api/states/all";

/// The header a `429` carries its delay in.
const RETRY_HEADER: &str = "x-rate-limit-retry-after-seconds";

/// The header that says how much of today's budget is left.
const REMAINING_HEADER: &str = "x-rate-limit-remaining";

/// The poll interval OpenSky's resolution supports without a credential.
pub const ANONYMOUS_INTERVAL: Duration = Duration::from_secs(10);

/// The poll interval an authenticated client gets.
pub const AUTHENTICATED_INTERVAL: Duration = Duration::from_secs(5);

/// The OpenSky Network, read over a bounding box.
#[derive(Debug)]
pub struct OpenSkyFeed {
    client: reqwest::Client,
    url: reqwest::Url,
    tokens: Tokens,
    state: SourceState,
}

impl OpenSkyFeed {
    /// Points OpenSky at the box this sidecar is watching.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the HTTP client or the URL
    /// cannot be built.
    pub fn open(
        area: Area,
        client_id: Option<String>,
        client_secret: Option<Secret>,
        interval: Option<Duration>,
    ) -> Result<Self, Error> {
        let tokens = Tokens::new(client_id, client_secret)?;
        let interval = interval.unwrap_or(if tokens.authenticated() {
            AUTHENTICATED_INTERVAL
        } else {
            ANONYMOUS_INTERVAL
        });

        let (south, west, north, east) = bounds(area);
        let mut url = reqwest::Url::parse(STATES_URL)
            .or_system_err(rustak_core::errors::ADVICE_REPORT_DEV)?;
        url.query_pairs_mut()
            .append_pair("lamin", &south.to_string())
            .append_pair("lomin", &west.to_string())
            .append_pair("lamax", &north.to_string())
            .append_pair("lomax", &east.to_string())
            .append_pair("extended", "1");

        announce(
            tokens.authenticated(),
            interval,
            credits(south, west, north, east),
        );

        Ok(Self {
            client: http_client()?,
            url,
            tokens,
            state: SourceState::new("opensky-network.org", interval),
        })
    }

    /// One request, renewing the token once if it turns out to be stale.
    async fn fetch(&mut self) -> Result<Vec<Track>, Error> {
        self.tokens.ensure(&self.client, false).await;

        let mut renewed = false;

        loop {
            let response = self.send().await?;
            let status = response.status();

            if status == reqwest::StatusCode::UNAUTHORIZED
                && self.tokens.authenticated()
                && !renewed
            {
                // Once per poll at most, so `debug`: a token that has expired
                // early is ordinary, and a token the server will never take is
                // said once by `Tokens::ensure`.
                debug!("OpenSky refused our token; renewing it and trying again.");
                renewed = true;
                self.tokens.ensure(&self.client, true).await;

                continue;
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                self.state
                    .wait_for(retry_after(response.headers(), RETRY_HEADER));

                return Ok(Vec::new());
            }

            if let Some(remaining) = response.headers().get(REMAINING_HEADER) {
                debug!(remaining = ?remaining, "OpenSky credits left today.");
            }

            let response = response
                .error_for_status()
                .wrap_user_err(format!("OpenSky answered {status}."), ADVICE_CREDENTIAL)?;

            return self.read(response).await;
        }
    }

    /// Sends the request, with the bearer token when there is one.
    async fn send(&self) -> Result<reqwest::Response, Error> {
        let mut request = self.client.get(self.url.clone());

        if let Some(token) = self.tokens.bearer() {
            request = request.bearer_auth(token);
        }

        request.send().await.wrap_user_err(
            "We could not reach the OpenSky Network.",
            &[
                "Check that this machine can reach the internet.",
                "OpenSky is a research network; it is sometimes down for maintenance.",
            ],
        )
    }

    /// Reads a successful response into tracks.
    async fn read(&mut self, response: reqwest::Response) -> Result<Vec<Track>, Error> {
        let body = response.text().await.wrap_user_err(
            "OpenSky sent a broken reply.",
            rustak_core::errors::ADVICE_RESTART_AFTER_FIXING,
        )?;
        let states: OpenSkyStates = serde_json::from_str(&body).wrap_user_err(
            "OpenSky sent something we could not read as state vectors.",
            &["The API may have changed shape; check the crate's README."],
        )?;

        self.state.succeeded();

        // The server's own clock, so that a position's age is measured against
        // the clock that stamped it.
        let now = DateTime::from_timestamp(states.time, 0).unwrap_or_else(Utc::now);

        Ok(states
            .states
            .unwrap_or_default()
            .iter()
            .filter_map(|state| track_from_state(state, now))
            .collect())
    }
}

/// The box to ask for, as OpenSky's four query parameters.
fn bounds(area: Area) -> (f64, f64, f64, f64) {
    let Area::Bbox {
        south,
        west,
        north,
        east,
    } = area.bbox()
    else {
        // `Area::bbox` answers a `Bbox` for both variants; written out rather
        // than unwrapped so that a new variant is a compile error here.
        return (-90.0, -180.0, 90.0, 180.0);
    };

    if west > east {
        warn!(
            "This area crosses the anti-meridian, which OpenSky's box cannot express; \
             asking for every longitude and filtering locally.",
        );

        return (south, -180.0, north, 180.0);
    }

    (south, west, north, east)
}

/// What one request over this box costs, in OpenSky credits.
fn credits(south: f64, west: f64, north: f64, east: f64) -> u32 {
    match (north - south).abs() * (east - west).abs() {
        area if area <= 25.0 => 1,
        area if area <= 100.0 => 2,
        area if area <= 400.0 => 3,
        _ => 4,
    }
}

/// Says at start-up what this configuration will cost, and warns when it is
/// more than the cheapest thing it could have been.
fn announce(authenticated: bool, interval: Duration, credits: u32) {
    let budget = if authenticated { 4_000 } else { 400 };
    let per_day = 86_400 / interval.as_secs().max(1) * u64::from(credits);

    info!(
        authenticated,
        seconds = interval.as_secs(),
        credits_per_request = credits,
        credits_per_day = per_day,
        daily_budget = budget,
        "Reading aircraft from the OpenSky Network (research and non-commercial use).",
    );

    if credits > 1 {
        warn!(
            credits_per_request = credits,
            "This area is larger than 25 square degrees, so every OpenSky request costs more \
             than one credit. A smaller area or a longer poll interval makes the budget last.",
        );
    }

    if per_day > budget {
        warn!(
            credits_per_day = per_day,
            daily_budget = budget,
            "At this interval the daily OpenSky budget runs out before the day does.",
        );
    }
}

#[async_trait]
impl Feed for OpenSkyFeed {
    fn name(&self) -> &str {
        self.state.name()
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        if !self.state.ready() {
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

impl AdsbFeed for OpenSkyFeed {
    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE: &str = include_str!("../../tests/fixtures/opensky.json");

    /// A feed whose two endpoints are a mock rather than OpenSky.
    fn against(server: &MockServer, authenticated: bool) -> OpenSkyFeed {
        let (client_id, secret) = if authenticated {
            (Some("rustak-test".to_string()), Some(Secret::new("shhh")))
        } else {
            (None, None)
        };

        let mut feed = OpenSkyFeed::open(
            Area::Bbox {
                south: 51.0,
                west: -1.0,
                north: 52.0,
                east: 0.0,
            },
            client_id,
            secret,
            Some(Duration::from_secs(0)),
        )
        .expect("it opens");

        feed.url =
            reqwest::Url::parse(&format!("{}/api/states/all", server.uri())).expect("a mock URL");
        feed.tokens.url =
            reqwest::Url::parse(&format!("{}/auth/token", server.uri())).expect("a mock token URL");

        feed
    }

    /// A mock that answers the token endpoint and the states endpoint.
    async fn network(states: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"access_token":"first-token","expires_in":1800,"token_type":"bearer"}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/states/all"))
            .respond_with(states)
            .mount(&server)
            .await;

        server
    }

    #[test]
    fn a_box_becomes_the_four_query_parameters_opensky_takes() {
        assert_eq!(
            bounds(Area::Bbox {
                south: 50.5,
                west: -2.0,
                north: 52.5,
                east: 1.0
            }),
            (50.5, -2.0, 52.5, 1.0),
        );
    }

    #[test]
    fn a_circle_asks_for_the_box_that_encloses_it() {
        let (south, west, north, east) = bounds(Area::Circle {
            lat: 51.0,
            lon: 0.0,
            radius_km: 111.32,
        });

        assert!((south - 50.0).abs() < 0.01, "{south}");
        assert!((north - 52.0).abs() < 0.01, "{north}");
        assert!(west < 0.0 && east > 0.0, "{west}..{east}");
    }

    #[test]
    fn an_anti_meridian_box_asks_for_every_longitude_instead() {
        // OpenSky's box has no way to say "west of -178 or east of 175", so the
        // alternative to the whole planet is nothing at all.
        let (south, west, north, east) = bounds(Area::Bbox {
            south: -20.0,
            west: 175.0,
            north: -15.0,
            east: -178.0,
        });

        assert_eq!((south, west, north, east), (-20.0, -180.0, -15.0, 180.0));
    }

    #[test]
    fn the_credit_cost_follows_the_published_bands() {
        assert_eq!(credits(51.0, -1.0, 52.0, 0.0), 1, "1 square degree");
        assert_eq!(
            credits(50.0, -2.0, 55.0, 3.0),
            1,
            "25 square degrees exactly"
        );
        assert_eq!(credits(50.0, -3.0, 56.0, 3.0), 2, "36 square degrees");
        assert_eq!(credits(40.0, -10.0, 50.0, 5.0), 3, "150 square degrees");
        assert_eq!(credits(-90.0, -180.0, 90.0, 180.0), 4, "the whole world");
    }

    #[test]
    fn an_anonymous_feed_polls_at_the_anonymous_resolution() {
        let feed = OpenSkyFeed::open(Area::default(), None, None, None).expect("it opens");

        assert_eq!(feed.state().interval(), ANONYMOUS_INTERVAL);
    }

    #[test]
    fn a_credential_buys_the_faster_resolution() {
        let feed = OpenSkyFeed::open(
            Area::default(),
            Some("rustak".to_string()),
            Some(Secret::new("shhh")),
            None,
        )
        .expect("it opens");

        assert_eq!(feed.state().interval(), AUTHENTICATED_INTERVAL);
    }

    #[test]
    fn half_a_credential_is_no_credential() {
        let feed = OpenSkyFeed::open(Area::default(), Some("rustak".to_string()), None, None)
            .expect("it opens");

        assert!(!feed.tokens.authenticated());
        assert_eq!(feed.state().interval(), ANONYMOUS_INTERVAL);
    }

    #[test]
    fn the_secret_never_appears_in_a_debug_rendering() {
        let feed = OpenSkyFeed::open(
            Area::default(),
            Some("rustak".to_string()),
            Some(Secret::new("hunter2")),
            None,
        )
        .expect("it opens");

        let rendered = format!("{feed:?}");

        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("Secret(***)"), "{rendered}");
    }

    #[tokio::test]
    async fn state_vectors_become_tracks() {
        let server = network(ResponseTemplate::new(200).set_body_string(FIXTURE)).await;
        let mut feed = against(&server, false);

        let tracks = feed.poll().await.expect("OpenSky answers");

        assert_eq!(tracks.len(), 2, "one has no position and one is stale");
        assert_eq!(tracks[0].id, "ADSB-3c6444");
        assert_eq!(tracks[0].callsign.as_deref(), Some("BAW117"));
        assert!(feed.state().is_connected());
    }

    #[tokio::test]
    async fn an_authenticated_feed_fetches_a_token_and_sends_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"access_token":"first-token","expires_in":1800}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer first-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let mut feed = against(&server, true);

        assert_eq!(feed.poll().await.expect("it answers").len(), 2);
        assert!(feed.tokens.held());
    }

    #[tokio::test]
    async fn a_401_renews_the_token_and_retries_the_request() {
        let server = MockServer::start().await;

        // Two tokens, in order: the stale one the first request is refused
        // with, and the good one the retry succeeds with.
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"access_token":"stale-token","expires_in":1800}"#),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"access_token":"fresh-token","expires_in":1800}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer stale-token"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer fresh-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let mut feed = against(&server, true);

        assert_eq!(
            feed.poll().await.expect("the retry succeeds").len(),
            2,
            "401 -> refresh -> retry, without the plugin noticing",
        );
    }

    #[tokio::test]
    async fn a_429_is_waited_out_for_as_long_as_the_header_asks() {
        let server = network(ResponseTemplate::new(429).insert_header(RETRY_HEADER, "45")).await;
        let mut feed = against(&server, false);
        feed.state.succeeded();

        assert!(feed.poll().await.expect("429 is not an error").is_empty());
        assert!(!feed.state().ready(), "we are waiting the 45 seconds out");
        assert!(
            feed.state().is_connected(),
            "they answered; they just said later"
        );
    }

    #[tokio::test]
    async fn a_token_endpoint_that_is_down_falls_back_to_anonymous() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let mut feed = against(&server, true);

        assert_eq!(
            feed.poll().await.expect("anonymous still works").len(),
            2,
            "a token endpoint having a bad minute is not a dark feed",
        );
        assert!(!feed.tokens.held());
    }

    #[tokio::test]
    async fn the_interval_is_a_floor_under_the_request_rate() {
        let server = network(ResponseTemplate::new(200).set_body_string(FIXTURE)).await;
        let mut feed = against(&server, false);
        feed.state = SourceState::new("opensky-network.org", Duration::from_secs(60));

        for _ in 0..4 {
            let _ = feed.poll().await;
        }

        let requests = server.received_requests().await.expect("a log");
        let states = requests
            .iter()
            .filter(|request| request.url.path() == "/api/states/all")
            .count();

        assert_eq!(states, 1, "four polls inside one interval is one request");
    }
}
