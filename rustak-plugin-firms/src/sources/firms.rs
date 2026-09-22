//! NASA FIRMS' area API: active-fire detections as CSV, by sensor and box.
//!
//! ```text
//! GET {base}/api/area/csv/{MAP_KEY}/{SOURCE}/{west,south,east,north | world}/{days}
//! ```
//!
//! One request per [`Sensor`] per box, on the source's own schedule rather
//! than the sidecar's tick. A box across the anti-meridian is two requests,
//! because FIRMS wants `west < east`; the whole world is the literal `world`.
//!
//! # The MAP_KEY is in the URL
//!
//! That is FIRMS' design, and it means the key is one careless log line away
//! from a journal. So: the URL is never logged, every `reqwest` error has its
//! URL stripped before it becomes a message, and any text FIRMS sends back is
//! passed through [`FirmsFeed::redact`] in case it echoes the key.
//!
//! # Being a guest
//!
//! A MAP_KEY buys 5000 transactions per ten minutes, and a request for a large
//! area or several days costs more than one. The satellites pass a few times a
//! day, so the default poll is ten minutes and nothing a configuration says
//! makes it faster than [`POLL_FLOOR`]. A `429` is waited out for as long as
//! it asks.

use std::time::Duration;

use rustak_client::feed::Area;
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{HotspotFeed, SourceState, http_client, retry_after};
use crate::wire::{self, Detection};

/// Where FIRMS lives, unless a configuration names a mirror or a proxy.
pub const DEFAULT_BASE_URL: &str = "https://firms.modaps.eosdis.nasa.gov";

/// How often FIRMS is asked when a configuration does not say.
pub const DEFAULT_POLL: Duration = Duration::from_secs(600);

/// The fastest FIRMS is asked, whatever a configuration says.
pub const POLL_FLOOR: Duration = Duration::from_secs(60);

/// The most days one request may cover, which is FIRMS' own ceiling.
pub const MAX_DAYS: u8 = 5;

const ADVICE_REFUSED: &[&str] = &[
    "Check `[settings.source] map_key`; a key is free at https://firms.modaps.eosdis.nasa.gov/api/map_key/.",
    "A key allows 5000 transactions per ten minutes, and a large area or several days costs more than one.",
];

const ADVICE_UNREACHABLE: &[&str] = &[
    "Check that this machine can reach firms.modaps.eosdis.nasa.gov over HTTPS.",
    "FIRMS is somebody else's service; it may simply be down.",
];

/// Which instrument's detections to read. Each is one request per poll.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sensor {
    /// VIIRS on Suomi NPP: 375 m pixels, global.
    ViirsSnpp,
    /// VIIRS on NOAA-20: 375 m pixels, global.
    ViirsNoaa20,
    /// VIIRS on NOAA-21: 375 m pixels, global.
    ViirsNoaa21,
    /// MODIS on Terra and Aqua: 1 km pixels, global.
    Modis,
    /// OLI on Landsat 8 and 9: 30 m pixels, the US and Canada only.
    Landsat,
}

impl Sensor {
    /// The near-real-time product's name in a FIRMS URL.
    #[must_use]
    pub const fn source_id(self) -> &'static str {
        match self {
            Self::ViirsSnpp => "VIIRS_SNPP_NRT",
            Self::ViirsNoaa20 => "VIIRS_NOAA20_NRT",
            Self::ViirsNoaa21 => "VIIRS_NOAA21_NRT",
            Self::Modis => "MODIS_NRT",
            Self::Landsat => "LANDSAT_NRT",
        }
    }
}

/// What one request came back with.
enum Reply {
    Detections(Vec<Detection>),
    /// A `429`, and how long it asked for.
    Wait(Option<Duration>),
}

/// FIRMS, read over an area.
#[derive(Debug)]
pub struct FirmsFeed {
    key: Secret,
    base: String,
    client: reqwest::Client,
    sensors: Vec<Sensor>,
    areas: Vec<String>,
    days: u8,
    state: SourceState,
}

impl FirmsFeed {
    /// Points FIRMS at the area this sidecar is watching.
    ///
    /// `days` and `poll` are clamped into what FIRMS accepts rather than
    /// refused, and the clamp is logged.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error for a MAP_KEY that cannot be one,
    /// an empty sensor list or a `base_url` that is not a URL. None of them
    /// echoes the key.
    pub fn open(
        key: Secret,
        sensors: &[Sensor],
        days: u8,
        poll: Option<Duration>,
        base_url: Option<&str>,
        area: Area,
    ) -> Result<Self, Error> {
        // The key becomes a path segment, so anything that is not plainly one
        // is refused here rather than sent somewhere surprising.
        if key.is_empty() || !key.expose().chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(human_errors::user(
                "`[settings.source] map_key` is not a FIRMS MAP_KEY: it is empty, or holds something other than letters and digits.",
                ADVICE_REFUSED,
            ));
        }

        if sensors.is_empty() {
            return Err(human_errors::user(
                "`[settings.source] sensors` is empty, so there is nothing to ask FIRMS for.",
                &["Name at least one sensor, or remove the key to get the default."],
            ));
        }

        let base = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
        reqwest::Url::parse(base).wrap_user_err(
            format!("`[settings.source] base_url` ('{base}') is not a URL."),
            &["Remove it to use FIRMS itself, or write it as https://host[:port]."],
        )?;

        let interval = poll.unwrap_or(DEFAULT_POLL).max(POLL_FLOOR);
        let clamped_days = days.clamp(1, MAX_DAYS);
        let areas = area_segments(area);

        if poll.is_some_and(|asked| asked < interval) || clamped_days != days {
            info!(
                poll_s = interval.as_secs(),
                days = clamped_days,
                "The configured poll or days is outside what this source will ask FIRMS for; using the nearest it will.",
            );
        }

        info!(
            ?sensors,
            ?areas,
            days = clamped_days,
            poll_s = interval.as_secs(),
            requests_per_poll = sensors.len() * areas.len(),
            "Reading active-fire detections from NASA FIRMS. FIRMS asks that its data be acknowledged: https://www.earthdata.nasa.gov/data/tools/firms",
        );

        Ok(Self {
            key,
            base: base.to_string(),
            client: http_client()?,
            sensors: sensors.to_vec(),
            areas,
            days: clamped_days,
            state: SourceState::new("NASA FIRMS", interval),
        })
    }

    /// `text`, with the MAP_KEY blanked wherever it appears.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        text.replace(self.key.expose(), "***")
    }

    /// The request for one sensor over one box. **Never log this.**
    fn url(&self, sensor: Sensor, area: &str) -> String {
        format!(
            "{}/api/area/csv/{}/{}/{area}/{}",
            self.base,
            self.key.expose(),
            sensor.source_id(),
            self.days,
        )
    }

    /// Every sensor over every box, stopping at the first `429`.
    async fn gather(&self) -> Result<(Vec<Detection>, Option<Reply>), Error> {
        let mut detections = Vec::new();

        for sensor in &self.sensors {
            for area in &self.areas {
                match self.request(*sensor, area).await? {
                    Reply::Detections(found) => detections.extend(found),
                    wait @ Reply::Wait(_) => return Ok((detections, Some(wait))),
                }
            }
        }

        Ok((detections, None))
    }

    /// One request, turned into detections or into a reason there are none.
    async fn request(&self, sensor: Sensor, area: &str) -> Result<Reply, Error> {
        let response = self
            .client
            .get(self.url(sensor, area))
            .send()
            .await
            // The URL carries the key, and `reqwest` prints it in its errors.
            .map_err(reqwest::Error::without_url)
            .wrap_user_err("We could not reach NASA FIRMS.", ADVICE_UNREACHABLE)?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(Reply::Wait(retry_after(response.headers())));
        }

        let body = response
            .text()
            .await
            .map_err(reqwest::Error::without_url)
            .wrap_user_err("NASA FIRMS sent a broken reply.", ADVICE_UNREACHABLE)?;

        let refused = |why: &str| {
            human_errors::user(
                format!(
                    "NASA FIRMS refused the request for {}: {}",
                    sensor.source_id(),
                    self.redact(why),
                ),
                ADVICE_REFUSED,
            )
        };

        if !status.is_success() {
            let said = body.lines().next().unwrap_or_default();

            return Err(refused(&format!("{status} {said}")));
        }

        let parsed = wire::parse(&body).map_err(|said| refused(&said))?;

        if parsed.skipped > 0 {
            debug!(
                sensor = sensor.source_id(),
                skipped = parsed.skipped,
                "Some FIRMS rows could not be placed in space and time.",
            );
        }

        Ok(Reply::Detections(parsed.detections))
    }
}

/// The area as FIRMS spells it: `world`, one box, or two across the
/// anti-meridian. A circle asks for the box around it, and
/// [`crate::hotspots::Hotspots`] trims the corners back off.
#[must_use]
pub fn area_segments(area: Area) -> Vec<String> {
    if area == Area::default() {
        return vec!["world".to_string()];
    }

    let Area::Bbox {
        south,
        west,
        north,
        east,
    } = area.bbox()
    else {
        // `bbox` always answers a box; the world is the safe reading of
        // anything else.
        return vec!["world".to_string()];
    };

    let (south, north) = (south.clamp(-90.0, 90.0), north.clamp(-90.0, 90.0));
    let segment = |west: f64, east: f64| format!("{west:.4},{south:.4},{east:.4},{north:.4}");

    match west <= east {
        true => vec![segment(west, east)],
        false => vec![segment(west, 180.0), segment(-180.0, east)],
    }
}

#[async_trait]
impl HotspotFeed for FirmsFeed {
    fn name(&self) -> &str {
        self.state.name()
    }

    async fn poll(&mut self) -> Result<Vec<Detection>, Error> {
        if !self.state.ready() {
            return Ok(Vec::new());
        }

        match self.gather().await {
            Ok((detections, None)) => {
                self.state.succeeded();

                Ok(detections)
            }
            Ok((detections, Some(Reply::Wait(asked)))) => {
                self.state.wait_for(asked);

                Ok(detections)
            }
            Ok((detections, Some(Reply::Detections(_)))) => Ok(detections),
            Err(err) => {
                self.state.failed(err.to_string());

                Err(err)
            }
        }
    }

    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn open(key: &str, days: u8, poll: Option<Duration>) -> Result<FirmsFeed, Error> {
        FirmsFeed::open(
            Secret::new(key),
            &[Sensor::ViirsNoaa20],
            days,
            poll,
            None,
            Area::default(),
        )
    }

    #[test]
    fn each_sensor_is_named_in_snake_case_and_asks_for_its_nrt_product() {
        for (written, expected) in [
            ("\"viirs_snpp\"", "VIIRS_SNPP_NRT"),
            ("\"viirs_noaa20\"", "VIIRS_NOAA20_NRT"),
            ("\"viirs_noaa21\"", "VIIRS_NOAA21_NRT"),
            ("\"modis\"", "MODIS_NRT"),
            ("\"landsat\"", "LANDSAT_NRT"),
        ] {
            let sensor: Sensor = serde_json::from_str(written).expect(written);

            assert_eq!(sensor.source_id(), expected);
        }
    }

    #[test]
    fn an_area_is_spelled_the_way_firms_reads_one() {
        let fiji = Area::Bbox {
            south: -19.0,
            west: 176.0,
            north: -16.0,
            east: -178.0,
        };
        let iberia = Area::Bbox {
            south: 36.0,
            west: -9.5,
            north: 43.8,
            east: 3.3,
        };

        assert_eq!(area_segments(Area::default()), ["world"]);
        assert_eq!(area_segments(iberia), ["-9.5000,36.0000,3.3000,43.8000"]);
        assert_eq!(
            area_segments(fiji),
            [
                "176.0000,-19.0000,180.0000,-16.0000",
                "-180.0000,-19.0000,-178.0000,-16.0000",
            ],
            "FIRMS wants west < east, so the anti-meridian is two requests",
        );

        let circle = area_segments(Area::Circle {
            lat: 40.0,
            lon: -8.0,
            radius_km: 100.0,
        });

        assert_eq!(circle.len(), 1);
        assert!(
            circle[0].starts_with("-9."),
            "the box around it: {circle:?}"
        );
    }

    #[test]
    fn what_firms_would_not_accept_is_clamped_rather_than_refused() {
        let feed = open(KEY, 30, Some(Duration::from_secs(1))).expect("it opens");

        assert_eq!(feed.days, MAX_DAYS);
        assert_eq!(feed.state.interval(), POLL_FLOOR);
        assert_eq!(open(KEY, 2, None).unwrap().state.interval(), DEFAULT_POLL);
    }

    #[test]
    fn a_key_that_cannot_be_one_is_refused_without_being_repeated() {
        for bad in ["", "not/a key", "../../etc/passwd"] {
            let err = open(bad, 1, None).expect_err(bad);

            assert!(err.to_string().contains("map_key"), "{err}");
            assert!(bad.is_empty() || !err.to_string().contains(bad), "{err}");
        }
    }

    #[test]
    fn the_key_is_in_the_url_and_nowhere_a_person_reads() {
        let feed = open(KEY, 1, None).expect("it opens");

        assert_eq!(
            feed.url(Sensor::ViirsNoaa20, "world"),
            format!("{DEFAULT_BASE_URL}/api/area/csv/{KEY}/VIIRS_NOAA20_NRT/world/1"),
        );
        assert!(!format!("{feed:?}").contains(KEY));
        assert_eq!(
            feed.redact(&format!("Invalid MAP_KEY: {KEY}")),
            "Invalid MAP_KEY: ***"
        );
    }
}
