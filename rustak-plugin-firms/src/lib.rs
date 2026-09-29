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

use chrono::{DateTime, Utc};
use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::Area;
use rustak_client::sidecar::{ServiceSettings, Sidecar, SidecarContext, SidecarEvent, async_trait};
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

    /// The harness's context, kept so that the area an administrator sets can
    /// be picked up after start-up as well as during it.
    context: Option<SidecarContext<Settings>>,

    /// The server's copy of this service's configuration, and when it is next
    /// worth reading.
    configured: ServiceSettings,

    /// The area in effect right now, and where it came from.
    area: Area,
    area_from: &'static str,
}

/// What [`FirmsSidecar::area`] says for each of the two places an area comes
/// from.
pub const FROM_FILE: &str = "the configuration file";
/// See [`FROM_FILE`].
pub const FROM_SERVER: &str = "an administrator, through the control API";

impl FirmsSidecar {
    /// The area in effect, and where it came from: [`FROM_FILE`] or
    /// [`FROM_SERVER`].
    #[must_use]
    pub const fn area(&self) -> (Area, &'static str) {
        (self.area, self.area_from)
    }

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

    /// The area an administrator set for this service, when the server holds
    /// a document this has not applied yet, and where that area comes from.
    ///
    /// `GET /api/v1/services/<name>/config` is a JSON object an administrator
    /// writes, so a deployment can move its area of interest from the admin UI
    /// — which for a fire season is the setting that changes. Only `area` is
    /// honoured. It is read at start-up and **again after it**, because the
    /// read at start-up is the one most likely to fail: the control link may
    /// not hold a credential yet. [`ServiceSettings`] owns the cadence.
    ///
    /// A document that names no `area` gives the choice back to the file; one
    /// that names an unreadable one is a warning and what is already in effect.
    async fn configured_area(&mut self, now: DateTime<Utc>) -> Option<(Area, &'static str)> {
        let context = self.context.clone()?;
        let document = self.configured.refresh_at(&context, now).await?;

        let Some(area) = document.get("area").filter(|area| !area.is_null()) else {
            return Some((context.settings().area, FROM_FILE));
        };

        match serde_json::from_value::<Area>(area.clone()) {
            Ok(area) => Some((area, FROM_SERVER)),
            Err(err) => {
                warn!(
                    "The `area` an administrator set for this service is not one we can read ({err}); \
                     the one already in effect stays in effect.",
                );

                None
            }
        }
    }

    /// Moves the source and the map onto `area`, keeping the source's
    /// schedule: FIRMS is not asked again early because the area moved.
    fn apply_area(&mut self, area: Area, from: &'static str) {
        if let Some(feed) = &mut self.feed {
            feed.set_area(area);
        }
        if let Some(hotspots) = &mut self.hotspots {
            hotspots.set_area(area);
        }

        if area != self.area {
            info!(
                ?area,
                area_from = from,
                "The area this service watches has changed; FIRMS is asked about it from the next poll.",
            );
        }

        (self.area, self.area_from) = (area, from);
    }

    /// [`Sidecar::tick`], at an instant of the caller's choosing: what the
    /// server-side configuration and the counters line are timed against.
    ///
    /// # Errors
    ///
    /// None today: an upstream that fails is a quiet tick, never a stop.
    pub async fn tick_at(&mut self, now: DateTime<Utc>) -> Result<Vec<Event>, Error> {
        if let Some((area, from)) = self.configured_area(now).await
            && (area, from) != (self.area, self.area_from)
        {
            self.apply_area(area, from);
        }

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
            // the source's own state has already announced the failure once
            // and reminds an operator every five minutes while it lasts.
            Err(err) => debug!(source = feed.name(), "The FIRMS feed did not answer: {err}"),
        }

        hotspots.tick();
        hotspots.report_at(now);

        Ok(hotspots.drain())
    }
}

#[async_trait]
impl Sidecar for FirmsSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        self.context = Some(ctx.clone());
        let settings = ctx.settings();
        let (area, from) = self
            .configured_area(Utc::now())
            .await
            .unwrap_or((settings.area, FROM_FILE));

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
        (self.area, self.area_from) = (area, from);

        info!(
            uid = %ctx.identity().uid(),
            source = self.kind,
            ?area,
            area_from = from,
            shape = ?settings.display.shape,
            "The FIRMS sidecar is watching for fires.",
        );

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        self.tick_at(Utc::now()).await
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

    /// Starts the plugin over the fixture, with a control API whose first
    /// answer about this service's configuration is `first` and every one
    /// after it `then`.
    async fn started_with_control(
        first: wiremock::ResponseTemplate,
        then: wiremock::ResponseTemplate,
    ) -> (FirmsSidecar, wiremock::MockServer) {
        use wiremock::matchers::{method, path};

        let control = wiremock::MockServer::start().await;
        for (priority, response, once) in [(1, first, true), (2, then, false)] {
            let mock = wiremock::Mock::given(method("GET"))
                .and(path("/api/v1/services/firms/config"))
                .respond_with(response)
                .with_priority(priority);
            match once {
                true => mock.up_to_n_times(1).mount(&control).await,
                false => mock.mount(&control).await,
            }
        }

        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/hotspots.example.csv");
        let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
            "[service]\nname = \"firms\"\ntoken = \"rsk_a_service_token\"\n\n\
             [server]\ncontrol = \"{}\"\n\n\
             [settings.area]\nkind = \"circle\"\nlat = 40.0\nlon = -8.0\nradius_km = 100.0\n\n\
             [settings.source]\nkind = \"replay\"\npath = \"{fixture}\"\n",
            control.uri(),
        ))
        .expect("the configuration loads");

        let mut sidecar = FirmsSidecar::default();
        sidecar
            .start(
                SidecarContext::from_config(config, FirmsSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the replay file opens");

        (sidecar, control)
    }

    /// Five kilometres around the fixture's cluster of four.
    fn around_the_cluster() -> serde_json::Value {
        serde_json::json!({
            "area": { "kind": "circle", "lat": 40.10234, "lon": -7.91456, "radius_km": 5.0 },
        })
    }

    #[tokio::test]
    async fn an_area_whose_first_read_failed_is_applied_when_the_read_works() {
        // The production finding: the read at start-up failed, and an
        // administrator's area was not in effect for the life of the process.
        let (mut sidecar, control) = started_with_control(
            wiremock::ResponseTemplate::new(503),
            wiremock::ResponseTemplate::new(200).set_body_json(around_the_cluster()),
        )
        .await;

        assert_eq!(
            sidecar.area().1,
            FROM_FILE,
            "the file's area until the server's can be read",
        );

        let _ = sidecar
            .tick_at(Utc::now() + chrono::Duration::seconds(31))
            .await
            .expect("a tick");

        let (area, from) = sidecar.area();
        assert_eq!(from, FROM_SERVER);
        assert!(area.contains(40.10234, -7.91456) && !area.contains(40.31277, -8.20918));
        assert_eq!(sidecar.tracked(), 4, "only the cluster is on the map");
        assert_eq!(
            control.received_requests().await.map(|all| all.len()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn an_area_the_administrator_takes_away_gives_the_choice_back_to_the_file() {
        let (mut sidecar, _control) = started_with_control(
            wiremock::ResponseTemplate::new(200).set_body_json(around_the_cluster()),
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
        )
        .await;

        assert_eq!(sidecar.area().1, FROM_SERVER, "read at start-up");

        let _ = sidecar
            .tick_at(Utc::now() + chrono::Duration::minutes(6))
            .await
            .expect("a tick");

        assert_eq!(sidecar.area().1, FROM_FILE);
        assert!(sidecar.area().0.contains(40.31277, -8.20918));
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
