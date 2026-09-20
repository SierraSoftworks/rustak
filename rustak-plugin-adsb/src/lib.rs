//! `rustak-plugin-adsb` — aircraft on the map, from open ADS-B data.
//!
//! ADS-B is what aircraft broadcast about themselves on 1090 MHz: a 24-bit
//! ICAO address, a flight number, a position, an altitude, a ground speed and a
//! track, once a second or so. A receiver on a windowsill and a `readsb` decode
//! it for nothing, and public aggregators pool what thousands of those
//! receivers hear. This sidecar is what turns that into CoT tracks on a rustak
//! channel.
//!
//! # The sources
//!
//! | `[settings.source]` `kind` | What it reads |
//! |---|---|
//! | `readsb` | A `readsb`/`dump1090` decoder's own `aircraft.json`, by path or over HTTP |
//! | `aggregator` | adsb.lol, adsb.fi or airplanes.live — the same document, pooled |
//! | `opensky` | The OpenSky Network's state vectors, anonymously or behind OAuth2 |
//! | `replay` | A file of tracks, for demonstrations and for the test suite |
//!
//! Each is a [`sources::AdsbFeed`]: it owns its own reconnection, its own
//! backoff and its own rate limiting, and answers how it is doing so that the
//! sidecar's heartbeat can say more than "healthy". [`mapping`] is the only
//! place that knows anything about emitter categories, feet or knots;
//! everything after it is [`rustak_client::feed`], which neither knows nor
//! cares that any of this came off 1090 MHz.
//!
//! # Running it
//!
//! ```text
//! rustak-plugin-adsb --config plugin.toml [--env .env] [--check]
//! ```
//!
//! See `config.example.toml` for every setting with its default, `README.md`
//! for each source's terms, and `docs/plugins.md` for the sidecar contract this
//! follows.

pub mod health;
pub mod mapping;
pub mod settings;
pub mod sources;
pub mod wire;

use std::time::Instant;

use rustak_api::ServiceState;
use rustak_client::feed::{Area, FeedCounters, FeedPublisher};
use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::prelude::*;
use rustak_cot::Event;

pub use settings::{MIN_MOVE_M, STALE, Settings, Source};
pub use sources::{AdsbFeed, Provider};

/// The plugin: one upstream, one publisher, and what it has done so far.
#[derive(Default)]
pub struct AdsbSidecar {
    context: Option<SidecarContext<Settings>>,
    publisher: Option<FeedPublisher>,
    feed: Option<Box<dyn AdsbFeed>>,
    /// Which kind of source is open, for the heartbeat.
    kind: &'static str,
    /// When the last heartbeat of our own went out, and what it said.
    reported: Option<(Instant, ServiceState)>,
}

impl AdsbSidecar {
    /// What this feed has offered, published, suppressed and expired — for a
    /// heartbeat, or for a test that would rather assert than read a log.
    #[must_use]
    pub fn counters(&self) -> FeedCounters {
        self.publisher
            .as_ref()
            .map_or_else(FeedCounters::default, FeedPublisher::counters)
    }

    /// How many aircraft are on the map right now.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.publisher.as_ref().map_or(0, FeedPublisher::tracked)
    }

    /// How the upstream is doing, as the admin UI would see it.
    #[must_use]
    pub fn state(&self) -> ServiceState {
        self.feed.as_ref().map_or(ServiceState::Unknown, |feed| {
            health::service_state(feed.state())
        })
    }

    /// Tells the server what this sidecar is carrying and how its upstream is.
    ///
    /// Best-effort and rate-limited: a heartbeat that did not land is not a
    /// reason to stop publishing CoT, and one that says the same thing as the
    /// last is not worth a request. A change of state always goes out.
    async fn report(&mut self) {
        let (Some(context), Some(feed), Some(publisher)) =
            (self.context.clone(), &self.feed, &self.publisher)
        else {
            return;
        };
        let Some(control) = context.control() else {
            return;
        };

        let beat = health::heartbeat(
            self.kind,
            feed.state(),
            publisher.counters(),
            publisher.tracked(),
        );
        let due = self
            .reported
            .is_none_or(|(at, state)| state != beat.state || at.elapsed() >= health::REPEAT_AFTER);

        if !due {
            return;
        }

        self.reported = Some((Instant::now(), beat.state));

        if let Err(err) = control.heartbeat(&beat).await {
            debug!("The server did not take this sidecar's heartbeat: {err}");
        }
    }
}

/// The area an administrator set for this service, when there is one.
///
/// `GET /api/v1/services/<name>/config` is a JSON object an administrator
/// writes and the service reads, so a deployment can move an area of interest
/// from the admin UI without anybody editing a file on the sidecar's host. Only
/// `area` is honoured, and only at start-up; anything else in the document is
/// somebody else's setting and is left alone.
///
/// Every failure here is a [`None`]: no control API, nothing configured, a
/// document that is not the shape we expect. A sidecar must start with the
/// file's area rather than refuse to start because a server could not be
/// reached.
async fn configured_area(context: &SidecarContext<Settings>) -> Option<Area> {
    let control = context.control()?;
    let document = match control.config().await {
        Ok(document) => document,
        Err(err) => {
            debug!("No server-side configuration for this service: {err}");

            return None;
        }
    };

    let area = document.get("area")?.clone();

    match serde_json::from_value::<Area>(area) {
        Ok(area) => {
            info!(
                ?area,
                "Using the area an administrator set for this service; it wins over the file.",
            );

            Some(area)
        }
        Err(err) => {
            warn!(
                "The `area` an administrator set for this service is not one we can read ({err}); \
                 using the one in the configuration file.",
            );

            None
        }
    }
}

#[async_trait]
impl Sidecar for AdsbSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        let settings = ctx.settings();
        let area = configured_area(&ctx).await.unwrap_or(settings.area);

        // Before anything else: a source that cannot be opened is a setting the
        // operator got wrong, and the one thing `start` should refuse over.
        self.kind = settings.source.kind();
        self.feed = Some(settings.source.open(area)?);
        self.publisher =
            Some(FeedPublisher::new(settings.publish, settings.affiliation).with_area(area));

        info!(
            uid = %ctx.identity().uid(),
            source = self.kind,
            upstream = self.feed.as_ref().map(|feed| feed.name()),
            ?area,
            affiliation = ?settings.affiliation,
            "The ADS-B sidecar is watching.",
        );

        self.context = Some(ctx);

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        if let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) {
            match feed.poll().await {
                Ok(tracks) => {
                    for track in tracks {
                        publisher.offer(track);
                    }
                }
                // An upstream that is down, rate-limiting or restarting is an
                // ordinary Tuesday for an open feed: logged, never a stopped
                // sidecar. The aircraft it was carrying age out on their own
                // `stale`, and the heartbeat below says what happened.
                Err(err) => warn!(source = feed.name(), "The ADS-B feed did not answer: {err}"),
            }

            publisher.tick();
        }

        self.report().await;

        Ok(self
            .publisher
            .as_mut()
            .map(FeedPublisher::drain)
            .unwrap_or_default())
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        match event {
            SidecarEvent::Connected { endpoint } => {
                info!(%endpoint, "Connected; every aircraft will be published again.");

                // A reopened connection is a new subscription: the server has
                // none of what went down the old one, so the intervals that
                // would otherwise suppress the next report are cleared.
                if let Some(publisher) = &mut self.publisher {
                    publisher.refresh_all();
                }
            }
            SidecarEvent::Disconnected { reason } => {
                warn!(%reason, "The stream dropped; the harness will reconnect.");
            }
            _ => debug!("An event this sidecar does not handle."),
        }

        Ok(Vec::new())
    }

    async fn stop(&mut self) -> Result<(), Error> {
        let counters = self.counters();

        info!(
            source = self.kind,
            offered = counters.offered,
            published = counters.published,
            suppressed = counters.suppressed,
            expired = counters.expired,
            "The ADS-B sidecar is stopping.",
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::feed::{AircraftClass, Track, TrackKind};
    use rustak_client::sidecar::SidecarConfig;
    use std::path::PathBuf;

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// The demonstration fixture the example configuration names.
    const FIXTURE: &str = include_str!("../tracks.example.ndjson");

    fn config() -> SidecarConfig<Settings> {
        rustak_core::config::load_str(EXAMPLE).expect("config.example.toml should load")
    }

    /// Starts the plugin over a replay of the demonstration fixture.
    async fn started() -> AdsbSidecar {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(&path, FIXTURE).expect("the fixture lands");

        let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
            "[service]\nname = \"adsb\"\n\n[settings.source]\nkind = \"replay\"\npath = \"{}\"\n",
            path.display(),
        ))
        .expect("the configuration loads");

        let mut sidecar = AdsbSidecar::default();
        sidecar
            .start(
                SidecarContext::from_config(config, AdsbSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the replay file opens");

        sidecar
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        let config = config();

        assert_eq!(config.service.name.as_str(), "adsb");
        assert_eq!(
            config.settings.affiliation,
            rustak_client::feed::Affiliation::Unknown
        );
        assert_eq!(config.settings.source.kind(), "replay");
        assert!(config.settings.area.contains(51.4775, -0.4614));
        assert_eq!(
            config.settings.publish.stale(),
            STALE,
            "the example writes `stale` out; a partial table would not",
        );
        assert!(
            (config.settings.publish.min_move_m - MIN_MOVE_M).abs() < f64::EPSILON,
            "and `min_move_m`, for the same reason",
        );
    }

    #[test]
    fn an_absent_settings_table_still_gets_the_shorter_horizon() {
        assert_eq!(Settings::default().publish.stale(), STALE);
        assert!(matches!(
            Settings::default().source,
            Source::Replay { ref path } if path == &PathBuf::from("tracks.ndjson"),
        ),);
    }

    #[test]
    fn the_demonstration_fixture_is_one_the_replay_source_reads() {
        // The example file names this fixture, so a change to either that broke
        // the other would ship a plugin whose demonstration does not run.
        let tracks: Vec<Track> = FIXTURE
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| serde_json::from_str(line).expect("a fixture line"))
            .collect();

        assert_eq!(tracks.len(), 5);
        assert!(tracks.iter().all(|track| track.id.starts_with("ADSB-")));
        assert!(
            tracks
                .iter()
                .any(|track| track.kind == TrackKind::Aircraft(AircraftClass::CivilRotary)),
        );
        assert!(
            tracks
                .iter()
                .any(|track| track.kind == TrackKind::GroundVehicle && track.on_ground),
            "ADS-B carries airport surface vehicles too (emitter categories C1-C3)",
        );
        assert!(
            tracks.iter().any(|track| track.altitude_hae_m.is_some()),
            "an aircraft without an altitude is a track on the ground",
        );
    }

    #[tokio::test]
    async fn a_tick_publishes_the_replayed_aircraft_once() {
        let mut sidecar = started().await;

        let published = sidecar.tick().await.expect("the first tick publishes");

        assert_eq!(published.len(), 5);
        assert!(published.iter().all(|event| event.uid.starts_with("ADSB-")));
        assert!(
            published
                .iter()
                .all(|event| event.stale.millis() - event.time.millis() == 90_000),
        );
        assert_eq!(sidecar.tracked(), 5);

        // The second tick offers the same five observations and publishes none
        // of them: the policy is what makes a feed a rate rather than a flood.
        assert!(sidecar.tick().await.unwrap().is_empty());
        assert_eq!(sidecar.counters().published, 5);
        assert_eq!(sidecar.counters().suppressed, 5);

        sidecar.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_running_replay_reports_itself_healthy() {
        let sidecar = started().await;

        assert_eq!(sidecar.state(), ServiceState::Healthy);
    }

    #[test]
    fn a_sidecar_that_has_not_started_has_nothing_to_report() {
        let sidecar = AdsbSidecar::default();

        assert_eq!(sidecar.state(), ServiceState::Unknown);
        assert_eq!(sidecar.tracked(), 0);
        assert_eq!(sidecar.counters(), FeedCounters::default());
    }

    #[tokio::test]
    async fn a_reconnection_publishes_every_aircraft_again() {
        let mut sidecar = AdsbSidecar {
            publisher: Some(FeedPublisher::new(
                settings::default_publish(),
                rustak_client::feed::Affiliation::Unknown,
            )),
            ..AdsbSidecar::default()
        };

        let publisher = sidecar.publisher.as_mut().unwrap();
        publisher.offer(Track::new(
            "ADSB-3c6444",
            TrackKind::Aircraft(AircraftClass::CivilFixedWing),
            (51.4775, -0.4614),
            chrono::Utc::now(),
        ));
        let _ = publisher.drain();

        let republished = sidecar
            .on_event(SidecarEvent::Connected {
                endpoint: "ssl://tak.example.com:8089".to_string(),
            })
            .await
            .unwrap();

        assert!(republished.is_empty(), "the next tick carries them");

        let publisher = sidecar.publisher.as_mut().unwrap();

        assert!(
            publisher.offer(Track::new(
                "ADSB-3c6444",
                TrackKind::Aircraft(AircraftClass::CivilFixedWing),
                (51.4775, -0.4614),
                chrono::Utc::now(),
            )),
            "an interval that would have suppressed this was cleared",
        );
    }

    #[test]
    fn a_settings_table_without_a_source_is_refused_by_name() {
        let refused = rustak_core::config::load_str::<SidecarConfig<Settings>>(
            "[service]\nname = \"adsb\"\n\n[settings]\naffiliation = \"neutral\"\n",
        );

        let err = refused.expect_err("a sidecar with no upstream is not a sidecar");

        assert!(err.to_string().contains("source"), "{err}");
    }
}
