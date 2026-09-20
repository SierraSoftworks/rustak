//! `rustak-plugin-adsb` — aircraft on the map, from open ADS-B data.
//!
//! ADS-B is what aircraft broadcast about themselves on 1090 MHz: a 24-bit
//! ICAO address, a flight number, a position, an altitude, a ground speed and a
//! track, once a second or so. A receiver on a windowsill and a `readsb` decode
//! it for nothing, and public aggregators pool what thousands of those
//! receivers hear. This sidecar is what turns that into CoT tracks on a rustak
//! channel.
//!
//! # What is here, and what is not
//!
//! This is the M9-00 skeleton: the plugin, its settings and its wiring, with
//! [`Source::Replay`] — a file of tracks — as its only upstream. The real
//! sources (a local or remote `aircraft.json`, an aggregator's
//! `v2/point/{lat}/{lon}/{radius}`, OpenSky's `states/all` behind OAuth2)
//! arrive in M9-02 as further variants of [`Source`], and nothing else here
//! changes: the model, the CoT mapping, the area filter and the publishing rate
//! all live in [`rustak_client::feed`].
//!
//! # Running it
//!
//! ```text
//! rustak-plugin-adsb --config plugin.toml [--env .env] [--check]
//! ```
//!
//! See `config.example.toml` for every setting with its default, and
//! `docs/plugins.md` for the sidecar contract this follows.

use std::path::PathBuf;
use std::time::Duration;

use rustak_client::feed::{
    Affiliation, Area, Feed, FeedCounters, FeedPublisher, PublishPolicy, Replay,
};
use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::prelude::*;
use rustak_cot::Event;

/// How long an aircraft stays on a map without another report.
///
/// Shorter than the [`PublishPolicy`] default, which is set for AIS: an
/// aircraft reports once a second, so one that has said nothing for ninety
/// seconds has left the area, landed, or was never really there.
const STALE: Duration = Duration::from_secs(90);

/// Where this plugin reads observations from.
///
/// `#[serde(tag = "kind")]`, so a settings file names the upstream it means and
/// a variant added in M9-02 is an additive change to the file format:
///
/// ```toml
/// [settings.source]
/// kind = "replay"
/// path = "tracks.ndjson"
/// ```
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A file of tracks, replayed on every tick. What the demonstration and the
    /// integration suite use, and what proves the rest of the plugin works
    /// without an upstream to be down.
    Replay {
        /// The newline-delimited JSON file; see [`Replay`] for the format.
        path: PathBuf,
    },
}

impl Source {
    /// Opens the upstream this setting names.
    ///
    /// # Errors
    ///
    /// Whatever the source could not do, as something the operator can fix: a
    /// replay file that is missing or malformed names itself.
    pub fn open(&self) -> Result<Box<dyn Feed>, Error> {
        match self {
            Self::Replay { path } => Ok(Box::new(Replay::open(path)?)),
        }
    }
}

/// The file replayed when a configuration has no `[settings]` table at all.
fn default_source() -> Source {
    Source::Replay {
        path: PathBuf::from("tracks.ndjson"),
    }
}

/// The publishing policy an ADS-B deployment starts from.
fn default_publish() -> PublishPolicy {
    PublishPolicy::default().with_stale(STALE)
}

/// `[settings]` — what this sidecar watches, and how loudly it says so.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where the feed is looking. Default: the whole world.
    #[serde(default)]
    pub area: Area,

    /// How often an aircraft may be republished, and how long it lives.
    ///
    /// Default: the [`PublishPolicy`] defaults with a `stale` of 90 seconds.
    /// **A `[settings.publish]` table that omits `stale` gets the module
    /// default of two minutes instead**, which is why the example file writes
    /// it out: a partial table is filled in key by key, not from this function.
    #[serde(default = "default_publish")]
    pub publish: PublishPolicy,

    /// What these tracks are to the operator. Default: `unknown`, because open
    /// ADS-B says nothing about whose side an airframe is on.
    #[serde(default)]
    pub affiliation: Affiliation,

    /// The upstream. Required, because choosing one is the whole deployment
    /// decision.
    pub source: Source,
}

impl Default for Settings {
    /// Only reached by a configuration file with no `[settings]` table, which
    /// is a sidecar that has not been told where to look: it replays
    /// `tracks.ndjson` from the working directory, and says so by name when
    /// that file is not there.
    fn default() -> Self {
        Self {
            area: Area::default(),
            publish: default_publish(),
            affiliation: Affiliation::default(),
            source: default_source(),
        }
    }
}

/// The plugin: one upstream, one publisher, and what it has done so far.
#[derive(Default)]
pub struct AdsbSidecar {
    context: Option<SidecarContext<Settings>>,
    publisher: Option<FeedPublisher>,
    feed: Option<Box<dyn Feed>>,
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
}

#[async_trait]
impl Sidecar for AdsbSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        let settings = ctx.settings();

        // Before anything else: a source that cannot be opened is a setting the
        // operator got wrong, and the one thing `start` should refuse over.
        self.feed = Some(settings.source.open()?);
        self.publisher = Some(
            FeedPublisher::new(settings.publish, settings.affiliation).with_area(settings.area),
        );

        info!(
            uid = %ctx.identity().uid(),
            area = ?settings.area,
            affiliation = ?settings.affiliation,
            "The ADS-B sidecar is watching.",
        );

        self.context = Some(ctx);

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) else {
            return Ok(Vec::new());
        };

        match feed.poll().await {
            Ok(tracks) => {
                for track in tracks {
                    publisher.offer(track);
                }
            }
            // An upstream that is down, rate-limiting or restarting is an
            // ordinary Tuesday for an open feed: logged, never a stopped
            // sidecar. The aircraft it was carrying age out on their own
            // `stale`.
            Err(err) => warn!(source = feed.name(), "The ADS-B feed did not answer: {err}"),
        }

        publisher.tick();

        Ok(publisher.drain())
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

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// The demonstration fixture the example configuration names.
    const FIXTURE: &str = include_str!("../tracks.example.ndjson");

    fn config() -> SidecarConfig<Settings> {
        rustak_core::config::load_str(EXAMPLE).expect("config.example.toml should load")
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        let config = config();

        assert_eq!(config.service.name.as_str(), "adsb");
        assert_eq!(config.settings.affiliation, Affiliation::Unknown);
        assert_eq!(
            config.settings.source,
            Source::Replay {
                path: PathBuf::from("tracks.example.ndjson")
            }
        );
        assert!(config.settings.area.contains(51.4775, -0.4614));
        assert_eq!(
            config.settings.publish.stale(),
            STALE,
            "the example writes `stale` out; a partial table would not",
        );
    }

    #[test]
    fn an_absent_settings_table_still_gets_the_shorter_horizon() {
        assert_eq!(Settings::default().publish.stale(), STALE);
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
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(&path, FIXTURE).expect("the fixture lands");

        let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
            r#"
            [service]
            name = "adsb"

            [settings.source]
            kind = "replay"
            path = "{}"
            "#,
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

        let published = sidecar.tick().await.expect("the first tick publishes");

        assert_eq!(published.len(), 5);
        assert!(published.iter().all(|event| event.uid.starts_with("ADSB-")));
        assert!(
            published
                .iter()
                .all(|event| event.stale.millis() - event.time.millis() == 90_000),
        );

        // The second tick offers the same five observations and publishes none
        // of them: the policy is what makes a feed a rate rather than a flood.
        assert!(sidecar.tick().await.unwrap().is_empty());
        assert_eq!(sidecar.counters().published, 5);
        assert_eq!(sidecar.counters().suppressed, 5);

        sidecar.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_reconnection_publishes_every_aircraft_again() {
        let mut sidecar = AdsbSidecar {
            publisher: Some(FeedPublisher::new(default_publish(), Affiliation::Unknown)),
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
