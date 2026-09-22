//! `rustak-plugin-esb` — Irish power outages on the map, from ESB Networks'
//! PowerCheck.
//!
//! After a storm, where the power is out is the first thing anybody
//! coordinating a response wants to see. ESB Networks publishes it at
//! <https://powercheck.esbnetworks.ie>; this sidecar reads the API behind that
//! map and turns every outage into a coloured spot marker on a rustak channel:
//! red for a fault, orange for planned works, green for a restoration.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`sources`] | Where outages come from: PowerCheck, or a replay file |
//! | [`wire`], [`time`] | PowerCheck's JSON and its Irish wall-clock times |
//! | [`outage`] | One outage, and the CoT marker it becomes |
//! | [`publish`], [`scope`] | What goes out, how often, and for which area |
//! | [`health`] | What the Services page says about this sidecar |
//!
//! ```text
//! rustak-plugin-esb --config plugin.toml [--env .env] [--check]
//! ```
//!
//! See `config.example.toml` for every setting with its default, `README.md`
//! for the data's terms, and `docs/plugins.md` for the sidecar contract.

pub mod health;
pub mod outage;
pub mod publish;
pub mod scope;
pub mod settings;
pub mod sources;
pub mod time;
pub mod wire;

use rustak_api::Heartbeat;
use rustak_client::feed::Area;
use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::prelude::*;
use rustak_cot::Event;

pub use outage::{Outage, OutageKind};
pub use publish::{OutagePublisher, Summary};
pub use scope::Scope;
pub use settings::{Settings, Source};
pub use sources::OutageFeed;

/// The plugin: one upstream, one publisher.
#[derive(Default)]
pub struct EsbSidecar {
    publisher: Option<OutagePublisher>,
    feed: Option<Box<dyn OutageFeed>>,
    kind: &'static str,
}

impl EsbSidecar {
    /// What is on the map right now.
    #[must_use]
    pub fn summary(&self) -> Summary {
        self.publisher
            .as_ref()
            .map_or_else(Summary::default, OutagePublisher::summary)
    }

    /// The heartbeat the harness reports; [`None`] until a source is open.
    #[must_use]
    pub fn heartbeat(&self) -> Option<Heartbeat> {
        let (feed, publisher) = (self.feed.as_ref()?, self.publisher.as_ref()?);

        Some(health::heartbeat(
            self.kind,
            feed.state(),
            publisher.counters(),
            publisher.summary(),
        ))
    }
}

/// The `area` an administrator set for this service in the admin UI, when
/// there is one and it can be read. It wins over the file, at start-up only,
/// and every failure is a [`None`]: a sidecar starts with the file's area
/// rather than refusing to start because a server could not be reached.
async fn configured_area(context: &SidecarContext<Settings>) -> Option<Area> {
    let document = context.control()?.config().await.ok()?;
    let area = document.get("area")?.clone();

    match serde_json::from_value::<Area>(area) {
        Ok(area) => {
            info!(
                ?area,
                "Using the area an administrator set for this service."
            );

            Some(area)
        }
        Err(err) => {
            warn!(
                "The `area` an administrator set is not one we can read ({err}); using the file's."
            );

            None
        }
    }
}

#[async_trait]
impl Sidecar for EsbSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        let settings = ctx.settings();
        let area = configured_area(&ctx).await.unwrap_or(settings.area);
        let scope = Scope::new(area, settings.include.clone());

        self.kind = settings.source.kind();
        self.feed = Some(settings.source.open(scope.clone())?);
        self.publisher = Some(OutagePublisher::new(
            scope,
            settings.stale(),
            settings.refresh(),
        ));

        info!(
            uid = %ctx.identity().uid(),
            source = self.kind,
            ?area,
            include = ?settings.include,
            "The ESB outage sidecar is watching.",
        );

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) else {
            return Ok(Vec::new());
        };

        match feed.poll().await {
            Ok(outages) => publisher.offer(outages),
            // An upstream that is down is never a stopped sidecar. `debug`,
            // because the source's own state has already said so once.
            Err(err) => debug!(
                source = feed.name(),
                "The outage feed did not answer: {err}"
            ),
        }

        Ok(publisher.drain())
    }

    async fn health(&mut self) -> Option<Heartbeat> {
        self.heartbeat()
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        match event {
            SidecarEvent::Connected { endpoint } => {
                info!(%endpoint, "Connected; every outage will be published again.");

                // A reopened connection has none of what went down the old one.
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
        let counters = self
            .publisher
            .as_ref()
            .map(OutagePublisher::counters)
            .unwrap_or_default();

        info!(
            source = self.kind,
            published = counters.published,
            suppressed = counters.suppressed,
            expired = counters.expired,
            "The ESB outage sidecar is stopping.",
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_api::ServiceState;
    use rustak_client::sidecar::SidecarConfig;

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// Starts the plugin over a replay of the demonstration fixture.
    async fn started(settings: &str) -> EsbSidecar {
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/outages.example.ndjson");
        let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
            "[service]\nname = \"esb\"\n\n[settings]\n{settings}\n\n[settings.source]\nkind = \"replay\"\npath = \"{fixture}\"\n",
        ))
        .expect("the configuration loads");

        let mut sidecar = EsbSidecar::default();
        sidecar
            .start(
                SidecarContext::from_config(config, EsbSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the replay file opens");

        sidecar
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        let config: SidecarConfig<Settings> =
            rustak_core::config::load_str(EXAMPLE).expect("config.example.toml should load");

        assert_eq!(config.service.name.as_str(), "esb");
        assert_eq!(config.settings.source.kind(), "replay");
        assert_eq!(config.settings.stale(), settings::STALE);
        assert_eq!(config.settings.refresh(), settings::REFRESH);
        assert_eq!(config.settings.include, OutageKind::ALL.to_vec());
        assert!(
            config.settings.area.contains(51.8139, -8.3986),
            "Cork is in Ireland"
        );
    }

    #[tokio::test]
    async fn a_tick_publishes_every_replayed_outage_once() {
        let mut sidecar = started("").await;

        let published = sidecar.tick().await.expect("the first tick publishes");

        assert_eq!(published.len(), 5);
        assert!(published.iter().all(|event| event.uid.starts_with("ESB-")));
        assert!(sidecar.tick().await.expect("the second").is_empty());
        assert_eq!(sidecar.summary().fault, 3);

        sidecar.stop().await.expect("it stops");
    }

    #[tokio::test]
    async fn only_the_kinds_asked_for_reach_the_map() {
        let mut sidecar = started("include = [\"fault\"]").await;

        let published = sidecar.tick().await.expect("a tick");

        assert_eq!(published.len(), 3);
        assert_eq!(sidecar.summary().total(), 3);
    }

    #[tokio::test]
    async fn the_health_hook_says_what_is_on_the_map() {
        let mut sidecar = started("").await;
        let _ = sidecar.tick().await.expect("a tick");

        let beat = sidecar.health().await.expect("a started sidecar reports");

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["kind"], "replay");
        assert_eq!(beat.metrics["outages"]["fault"], 3);
        assert_eq!(beat.metrics["feed"]["published"], 5);
    }

    #[tokio::test]
    async fn a_reconnection_publishes_every_outage_again() {
        let mut sidecar = started("").await;
        let _ = sidecar.tick().await.expect("a tick");

        sidecar
            .on_event(SidecarEvent::Connected {
                endpoint: "ssl://tak.example.com:8089".to_string(),
            })
            .await
            .expect("handled");

        assert_eq!(sidecar.tick().await.expect("the next tick").len(), 5);
    }

    #[tokio::test]
    async fn a_sidecar_that_has_not_started_leaves_the_harness_its_floor() {
        assert!(EsbSidecar::default().health().await.is_none());
    }
}
