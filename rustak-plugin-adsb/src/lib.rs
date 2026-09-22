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

use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::{Area, FeedCounters, FeedPublisher};
use rustak_client::sidecar::{ServiceSettings, Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::prelude::*;
use rustak_cot::Event;

pub use settings::{MIN_MOVE_M, STALE, Settings, Source};
pub use sources::{AdsbFeed, Provider};

/// The plugin: one upstream, one publisher, and what it has done so far.
#[derive(Default)]
pub struct AdsbSidecar {
    publisher: Option<FeedPublisher>,
    feed: Option<Box<dyn AdsbFeed>>,
    /// Which kind of source is open, for the heartbeat.
    kind: &'static str,

    /// The harness's context, kept so that the area an administrator sets can
    /// be picked up after start-up as well as during it.
    context: Option<SidecarContext<Settings>>,

    /// The server's copy of this service's configuration, and when it is next
    /// worth reading.
    configured: ServiceSettings,

    /// The area in effect right now, whichever of the two it came from.
    area: Area,

    /// Where that area came from, for the line that says which is in effect.
    area_from: &'static str,
}

/// What [`AdsbSidecar::area_from`] says for each of the two.
const FROM_FILE: &str = "the configuration file";
const FROM_SERVER: &str = "an administrator, through the control API";

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

    /// What this sidecar is carrying and how its upstream is, as the heartbeat
    /// the harness reports.
    ///
    /// [`None`] until [`Sidecar::start`] has opened a source, which is the only
    /// window in which this plugin has nothing to say.
    #[must_use]
    pub fn heartbeat(&self) -> Option<Heartbeat> {
        let (feed, publisher) = (self.feed.as_ref()?, self.publisher.as_ref()?);

        Some(health::heartbeat(
            self.kind,
            feed.state(),
            publisher.counters(),
            publisher.tracked(),
        ))
    }

    /// The area an administrator set for this service, when they have set a new
    /// one.
    ///
    /// `GET /api/v1/services/<name>/config` is a JSON object an administrator
    /// writes and the service reads, so a deployment can move an area of
    /// interest from the admin UI without anybody editing a file on the
    /// sidecar's host. Only `area` is honoured; anything else in the document
    /// is somebody else's setting and is left alone.
    ///
    /// It is asked **again after start-up**, because the read at start-up is
    /// the one most likely to fail: the control link may not have a credential
    /// yet. The first live deployment lost an administrator's area that way,
    /// silently, for the whole life of the process.
    /// [`ServiceSettings`] owns the cadence.
    async fn configured_area(&mut self) -> Option<Area> {
        let context = self.context.clone()?;

        self.configured
            .setting::<Settings, Area>(&context, "area")
            .await
            .filter(|area| *area != self.area)
    }

    /// Opens the source and the publisher for `area`, and remembers where it
    /// came from.
    ///
    /// The same two lines whether this is a start or an administrator moving
    /// the box: a feed subscribes with its area, so changing one means opening
    /// the source again.
    fn watch(&mut self, settings: &Settings, area: Area, from: &'static str) -> Result<(), Error> {
        self.kind = settings.source.kind();
        self.feed = Some(settings.source.open(area)?);
        self.publisher =
            Some(
                FeedPublisher::new(settings.publish, settings.affiliation)
                    .with_symbology(settings.symbology)
                    .with_area(area),
            );
        self.area = area;
        self.area_from = from;

        Ok(())
    }
}

#[async_trait]
impl Sidecar for AdsbSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        self.area = ctx.settings().area;
        self.area_from = FROM_FILE;
        self.context = Some(ctx.clone());

        let (area, from) = match self.configured_area().await {
            Some(area) => (area, FROM_SERVER),
            None => (self.area, FROM_FILE),
        };
        let settings = ctx.settings();

        // Before anything else: a source that cannot be opened is a setting the
        // operator got wrong, and the one thing `start` should refuse over.
        self.watch(settings, area, from)?;

        info!(
            uid = %ctx.identity().uid(),
            source = self.kind,
            upstream = self.feed.as_ref().map(|feed| feed.name()),
            ?area,
            area_from = self.area_from,
            affiliation = ?settings.affiliation,
            symbology = ?settings.symbology,
            "The ADS-B sidecar is watching.",
        );

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        // An administrator may have moved the box since this started — or the
        // read that would have found it at start-up may simply not have worked
        // yet.
        if let Some(area) = self.configured_area().await
            && let Some(context) = self.context.clone()
        {
            match self.watch(context.settings(), area, FROM_SERVER) {
                Ok(()) => info!(
                    ?area,
                    area_from = self.area_from,
                    "The area an administrator set for this service is now in effect.",
                ),
                Err(err) => warn!(
                    error = %err,
                    "Could not open the source for the area an administrator set; the one already in effect stays in effect.",
                ),
            }
        }

        if let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) {
            match feed.poll().await {
                Ok(tracks) => {
                    for track in tracks {
                        publisher.offer(track);
                    }
                }
                // An upstream that is down, rate-limiting or restarting is an
                // ordinary Tuesday for an open feed: never a stopped sidecar.
                // The aircraft it was carrying age out on their own `stale`,
                // and the health hook says what happened.
                //
                // `debug`, because the source's own `SourceState` has already
                // announced this failure once and will remind an operator
                // every five minutes for as long as it lasts. A `warn` here as
                // well is the same outage twice, once per tick.
                Err(err) => debug!(source = feed.name(), "The ADS-B feed did not answer: {err}"),
            }

            publisher.tick();
        }

        Ok(self
            .publisher
            .as_mut()
            .map(FeedPublisher::drain)
            .unwrap_or_default())
    }

    /// The harness asks after every tick, and reports exactly this.
    ///
    /// Everything it answers was worked out during the tick that has just
    /// finished — the source's connection state, the publisher's counters —
    /// so this reads rather than polls.
    async fn health(&mut self) -> Option<Heartbeat> {
        self.heartbeat()
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
        assert_eq!(
            config.settings.symbology,
            rustak_client::feed::Symbology::TypeOnly
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

    #[tokio::test]
    async fn the_health_hook_is_what_the_harness_reports_for_this_plugin() {
        // The hook, not a request of our own: whatever this answers is the
        // heartbeat the Services page shows, so it has to carry the whole
        // picture rather than a floor.
        let mut sidecar = started().await;
        let _ = sidecar.tick().await.expect("a tick");

        let beat = sidecar.health().await.expect("a started sidecar reports");

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["kind"], "replay");
        assert_eq!(beat.metrics["tracked"], 5);
        assert_eq!(beat.metrics["feed"]["published"], 5);
        assert!(beat.message.is_some());
    }

    #[tokio::test]
    async fn a_sidecar_that_has_not_started_leaves_the_harness_its_floor() {
        // `None` is "nothing to add", which the harness reports as healthy —
        // the window between the process starting and `start` opening a source.
        assert!(AdsbSidecar::default().health().await.is_none());
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
