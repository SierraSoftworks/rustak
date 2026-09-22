//! `rustak-plugin-firms` — fires on the map, from NASA FIRMS.
//!
//! [FIRMS](https://firms.modaps.eosdis.nasa.gov) publishes the thermal
//! anomalies that the VIIRS, MODIS and Landsat instruments flag as active
//! fires, within hours of each overpass. This sidecar reads them over an area
//! of interest and publishes each one as CoT: a coloured marker, the polygon
//! of ground the satellite pixel covered, or both.
//!
//! # The pieces
//!
//! | Module | What it owns |
//! |---|---|
//! | [`sources`] | Where detections come from: the FIRMS area API, or a CSV replay |
//! | [`wire`] | The FIRMS CSV, read by header name into a [`wire::Detection`] |
//! | [`mapping`] | A detection as CoT: uid, marker, footprint, colour, remarks |
//! | [`hotspots`] | Which detections are on the map, and when each is said again |
//! | [`health`] | The heartbeat the Services page shows |
//!
//! # Running it
//!
//! ```text
//! rustak-plugin-firms --config plugin.toml [--env .env] [--check]
//! ```
//!
//! See `config.example.toml` for every setting with its default, `README.md`
//! for FIRMS' terms, and `docs/plugins.md` for the sidecar contract.

pub mod health;
pub mod hotspots;
pub mod mapping;
pub mod settings;
pub mod sources;
pub mod wire;

use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::Area;
use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::prelude::*;
use rustak_cot::Event;

pub use hotspots::{Counters, Hotspots};
pub use settings::{Settings, Source};
pub use sources::HotspotFeed;

/// The plugin: one upstream, one map of detections, and what it has done.
#[derive(Default)]
pub struct FirmsSidecar {
    hotspots: Option<Hotspots>,
    feed: Option<Box<dyn HotspotFeed>>,
    /// Which kind of source is open, for the heartbeat.
    kind: &'static str,
}

impl FirmsSidecar {
    /// What this feed has offered, published, republished, suppressed and
    /// expired.
    #[must_use]
    pub fn counters(&self) -> Counters {
        self.hotspots
            .as_ref()
            .map_or_else(Counters::default, Hotspots::counters)
    }

    /// How many detections are on the map right now.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.hotspots.as_ref().map_or(0, Hotspots::tracked)
    }

    /// How the upstream is doing, as the admin UI would see it.
    #[must_use]
    pub fn state(&self) -> ServiceState {
        self.feed.as_ref().map_or(ServiceState::Unknown, |feed| {
            health::service_state(feed.state())
        })
    }

    /// The heartbeat the harness reports, or [`None`] until
    /// [`Sidecar::start`] has opened a source.
    #[must_use]
    pub fn heartbeat(&self) -> Option<Heartbeat> {
        let (feed, hotspots) = (self.feed.as_ref()?, self.hotspots.as_ref()?);

        Some(health::heartbeat(
            self.kind,
            feed.state(),
            hotspots.counters(),
            hotspots.tracked(),
            hotspots.pending(),
        ))
    }
}

/// The area an administrator set for this service, when there is one.
///
/// `GET /api/v1/services/<name>/config` is a JSON object an administrator
/// writes, so a deployment can move its area of interest from the admin UI —
/// which for a fire season is the setting that changes. Only `area` is
/// honoured, and only at start-up.
///
/// Every failure here is a [`None`]: a sidecar starts with the file's area
/// rather than refusing to start because a server could not be reached.
async fn configured_area(context: &SidecarContext<Settings>) -> Option<Area> {
    let document = match context.control()?.config().await {
        Ok(document) => document,
        Err(err) => {
            debug!("No server-side configuration for this service: {err}");

            return None;
        }
    };

    match serde_json::from_value::<Area>(document.get("area")?.clone()) {
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
impl Sidecar for FirmsSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        let settings = ctx.settings();
        let area = configured_area(&ctx).await.unwrap_or(settings.area);

        // A source that cannot be opened is a setting the operator got wrong,
        // and the one thing `start` should refuse over.
        self.kind = settings.source.kind();
        self.feed = Some(settings.source.open(area)?);
        self.hotspots = Some(Hotspots::new(
            area,
            settings.filter,
            settings.display.clone(),
            settings.publish,
        ));

        info!(
            uid = %ctx.identity().uid(),
            source = self.kind,
            ?area,
            shape = ?settings.display.shape,
            "The FIRMS sidecar is watching for fires.",
        );

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        let (Some(feed), Some(hotspots)) = (&mut self.feed, &mut self.hotspots) else {
            return Ok(Vec::new());
        };

        match feed.poll().await {
            Ok(detections) => {
                for detection in detections {
                    hotspots.offer(detection);
                }
            }
            // An upstream that is down or refusing is never a stopped sidecar:
            // what is on the map ages out on its own `stale`. `debug`, because
            // the source's own state has already announced the failure once.
            Err(err) => debug!(source = feed.name(), "The FIRMS feed did not answer: {err}"),
        }

        hotspots.tick();

        Ok(hotspots.drain())
    }

    /// The harness asks after every tick, and reports exactly this.
    async fn health(&mut self) -> Option<Heartbeat> {
        self.heartbeat()
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        match event {
            SidecarEvent::Connected { endpoint } => {
                info!(%endpoint, "Connected; every detection will be published again.");

                // A reopened connection is a new subscription: the server has
                // none of what went down the old one. The next ticks carry it,
                // `max_per_tick` at a time.
                if let Some(hotspots) = &mut self.hotspots {
                    hotspots.refresh_all();
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
            republished = counters.republished,
            revised = counters.revised,
            suppressed = counters.suppressed,
            expired = counters.expired,
            "The FIRMS sidecar is stopping.",
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::sidecar::SidecarConfig;

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// The demonstration fixture the example configuration names.
    const FIXTURE: &str = include_str!("../hotspots.example.csv");

    /// Starts the plugin from the example configuration, with the fixture it
    /// names placed where a temporary working directory can reach it.
    async fn started() -> (FirmsSidecar, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("hotspots.example.csv");
        std::fs::write(&path, FIXTURE).expect("the fixture lands");

        let example = EXAMPLE.replace(
            "path = \"hotspots.example.csv\"",
            &format!("path = {:?}", path.display().to_string()),
        );
        let config: SidecarConfig<Settings> =
            rustak_core::config::load_str(&example).expect("config.example.toml should load");

        let mut sidecar = FirmsSidecar::default();
        sidecar
            .start(
                SidecarContext::from_config(config, FirmsSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the replay file opens");

        (sidecar, directory)
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        let config: SidecarConfig<Settings> =
            rustak_core::config::load_str(EXAMPLE).expect("config.example.toml should load");

        assert_eq!(config.service.name.as_str(), "firms");
        assert_eq!(config.settings.source.kind(), "replay");
        assert_eq!(
            config.settings.publish,
            hotspots::Publish::default(),
            "the example writes every default out",
        );
        assert_eq!(config.settings.display, mapping::Display::default());
    }

    #[tokio::test]
    async fn the_demonstration_publishes_its_fires_once_and_reports_them() {
        let (mut sidecar, _directory) = started().await;
        let fires = wire::parse(FIXTURE).expect("a FIRMS CSV").detections.len();

        let published = sidecar.tick().await.expect("the first tick publishes");

        assert!(fires > 0 && published.len() == fires, "{}", published.len());
        assert!(published.iter().all(|e| e.uid.starts_with(mapping::PREFIX)));
        assert!(sidecar.tick().await.unwrap().is_empty(), "nothing is new");

        let beat = sidecar.health().await.expect("a started sidecar reports");

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["kind"], "replay");
        assert_eq!(beat.metrics["tracked"], fires);

        sidecar.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_reconnection_publishes_every_detection_again() {
        let (mut sidecar, _directory) = started().await;
        let first = sidecar.tick().await.unwrap().len();

        let at_once = sidecar
            .on_event(SidecarEvent::Connected {
                endpoint: "ssl://tak.example.com:8089".to_string(),
            })
            .await
            .unwrap();

        assert!(at_once.is_empty(), "the next tick carries them");
        assert_eq!(sidecar.tick().await.unwrap().len(), first);
    }

    #[tokio::test]
    async fn a_sidecar_that_has_not_started_has_nothing_to_report() {
        let mut sidecar = FirmsSidecar::default();

        assert!(sidecar.health().await.is_none());
        assert_eq!(sidecar.state(), ServiceState::Unknown);
        assert_eq!(sidecar.tracked(), 0);
        assert!(sidecar.tick().await.unwrap().is_empty());
    }
}
