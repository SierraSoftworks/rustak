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

use chrono::{DateTime, Utc};
use rustak_api::Heartbeat;
use rustak_client::feed::Area;
use rustak_client::sidecar::{ServiceSettings, Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::prelude::*;
use rustak_cot::Event;

pub use outage::{Outage, OutageKind};
pub use publish::{OutagePublisher, Summary};
pub use scope::Scope;
pub use settings::{Settings, Source};
pub use sources::OutageFeed;

/// The plugin: one upstream, one publisher, and the area they are watching.
#[derive(Default)]
pub struct EsbSidecar {
    publisher: Option<OutagePublisher>,
    feed: Option<Box<dyn OutageFeed>>,
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

/// What [`EsbSidecar::area`] says for each of the two places an area comes from.
pub const FROM_FILE: &str = "the configuration file";
/// See [`FROM_FILE`].
pub const FROM_SERVER: &str = "an administrator, through the control API";

impl EsbSidecar {
    /// The area in effect, and where it came from: [`FROM_FILE`] or
    /// [`FROM_SERVER`].
    #[must_use]
    pub const fn area(&self) -> (Area, &'static str) {
        (self.area, self.area_from)
    }

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

    /// The area an administrator set for this service, when the server holds
    /// a document this has not applied yet, and where that area comes from.
    ///
    /// Read at start-up and **again after it**, because the read at start-up
    /// is the one most likely to fail: the control link may not hold a
    /// credential yet, which is how an override was silently not in effect for
    /// the whole life of a process. [`ServiceSettings`] owns the cadence. A
    /// document that names no `area` gives the choice back to the file; one
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
                    "The `area` an administrator set is not one we can read ({err}); the one already in effect stays in effect."
                );

                None
            }
        }
    }

    /// Moves the source and the publisher onto `area`, keeping the source's
    /// schedule: ESB is not asked again early because the area moved.
    fn apply_area(&mut self, area: Area, from: &'static str) {
        let Some(context) = &self.context else {
            return;
        };
        let scope = Scope::new(area, context.settings().include.clone());

        if let Some(feed) = &mut self.feed {
            feed.rescope(scope.clone());
        }
        if let Some(publisher) = &mut self.publisher {
            publisher.set_scope(scope);
        }

        if area != self.area {
            info!(
                ?area,
                area_from = from,
                "The area this service watches has changed; outages newly inside it arrive with the next list.",
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

        let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) else {
            return Ok(Vec::new());
        };

        match feed.poll().await {
            Ok(outages) => publisher.offer(outages),
            // An upstream that is down is never a stopped sidecar. `debug`,
            // because the source's own state has already said so once and
            // reminds an operator every five minutes while it lasts.
            Err(err) => debug!(
                source = feed.name(),
                "The outage feed did not answer: {err}"
            ),
        }

        publisher.report_at(now);

        Ok(publisher.drain())
    }
}

#[async_trait]
impl Sidecar for EsbSidecar {
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
        let scope = Scope::new(area, settings.include.clone());

        self.kind = settings.source.kind();
        self.feed = Some(settings.source.open(scope.clone())?);
        self.publisher = Some(OutagePublisher::new(
            scope,
            settings.stale(),
            settings.refresh(),
        ));
        (self.area, self.area_from) = (area, from);

        info!(
            uid = %ctx.identity().uid(),
            source = self.kind,
            ?area,
            area_from = from,
            include = ?settings.include,
            "The ESB outage sidecar is watching.",
        );

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        self.tick_at(Utc::now()).await
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// Starts the plugin over a replay of the demonstration fixture.
    async fn started(settings: &str) -> EsbSidecar {
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/outages.example.ndjson");

        started_over(&format!(
            "[settings]\n{settings}\n\n[settings.source]\nkind = \"replay\"\npath = \"{fixture}\"\n",
        ))
        .await
    }

    /// Starts the plugin over whatever `[settings]` an operator might write.
    async fn started_over(settings: &str) -> EsbSidecar {
        let config: SidecarConfig<Settings> =
            rustak_core::config::load_str(&format!("[service]\nname = \"esb\"\n\n{settings}"))
                .expect("the configuration loads");

        let mut sidecar = EsbSidecar::default();
        sidecar
            .start(
                SidecarContext::from_config(config, EsbSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the source opens");

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
    async fn an_upstream_that_fails_is_a_quiet_tick_and_a_heartbeat_that_says_why() {
        let esb = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&esb)
            .await;
        let mut sidecar = started_over(&format!(
            "[settings.source]\nkind = \"powercheck\"\napi_key = \"test-key\"\nbase_url = \"{}\"\n",
            esb.uri(),
        ))
        .await;

        let published = sidecar.tick().await.expect("never a stopped sidecar");
        let beat = sidecar.health().await.expect("a started sidecar reports");

        assert!(published.is_empty());
        assert_eq!(beat.state, ServiceState::Unhealthy, "it has never answered");
        assert!(
            beat.message
                .is_some_and(|message| message.contains("not answering"))
        );
    }

    #[tokio::test]
    async fn events_this_sidecar_does_not_act_on_publish_nothing() {
        let mut sidecar = started("").await;

        for event in [
            SidecarEvent::Disconnected {
                reason: "connection reset".to_string(),
            },
            SidecarEvent::Negotiated { protobuf: true },
        ] {
            assert!(sidecar.on_event(event).await.expect("handled").is_empty());
        }
    }

    /// A control API whose first answer about this service's configuration is
    /// `first`, and every one after it `then`.
    async fn control(first: ResponseTemplate, then: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;

        for (priority, response, times) in [(1, first, Some(1)), (2, then, None)] {
            let mock = Mock::given(method("GET"))
                .and(path("/api/v1/services/esb/config"))
                .respond_with(response)
                .with_priority(priority);
            match times {
                Some(times) => mock.up_to_n_times(times).mount(&server).await,
                None => mock.mount(&server).await,
            }
        }

        server
    }

    /// Starts the plugin over the fixture, with a control API at `control`.
    async fn started_with_control(control: &MockServer) -> EsbSidecar {
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/outages.example.ndjson");
        let config: SidecarConfig<Settings> = rustak_core::config::load_str(&format!(
            "[service]\nname = \"esb\"\ntoken = \"rsk_a_service_token\"\n\n\
             [server]\ncontrol = \"{}\"\n\n\
             [settings.source]\nkind = \"replay\"\npath = \"{fixture}\"\n",
            control.uri(),
        ))
        .expect("the configuration loads");

        let mut sidecar = EsbSidecar::default();
        sidecar
            .start(
                SidecarContext::from_config(config, EsbSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the source opens");

        sidecar
    }

    /// Twenty kilometres around Carrigaline: one of the fixture's five.
    fn around_cork() -> serde_json::Value {
        serde_json::json!({
            "area": { "kind": "circle", "lat": 51.8139, "lon": -8.3986, "radius_km": 20.0 },
        })
    }

    #[tokio::test]
    async fn an_area_whose_first_read_failed_is_applied_when_the_read_works() {
        // The production finding: the read at start-up failed, and an
        // administrator's area was not in effect for the life of the process.
        let control = control(
            ResponseTemplate::new(503),
            ResponseTemplate::new(200).set_body_json(around_cork()),
        )
        .await;
        let mut sidecar = started_with_control(&control).await;

        assert_eq!(
            sidecar.area(),
            (Area::default(), FROM_FILE),
            "the file's area until the server's can be read",
        );

        let _ = sidecar
            .tick_at(Utc::now() + chrono::Duration::seconds(31))
            .await
            .expect("a tick");

        let (area, from) = sidecar.area();
        assert_eq!(from, FROM_SERVER);
        assert!(area.contains(51.8139, -8.3986) && !area.contains(53.3498, -6.2603));
        assert_eq!(sidecar.summary().total(), 1, "only Carrigaline is held");
        assert_eq!(
            control.received_requests().await.map(|all| all.len()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn an_area_the_administrator_takes_away_gives_the_choice_back_to_the_file() {
        let control = control(
            ResponseTemplate::new(200).set_body_json(around_cork()),
            ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
        )
        .await;
        let mut sidecar = started_with_control(&control).await;

        assert_eq!(sidecar.area().1, FROM_SERVER, "read at start-up");

        let _ = sidecar
            .tick_at(Utc::now() + chrono::Duration::minutes(6))
            .await
            .expect("a tick");

        assert_eq!(sidecar.area(), (Area::default(), FROM_FILE));
    }

    #[tokio::test]
    async fn a_sidecar_that_has_not_started_leaves_the_harness_its_floor() {
        assert!(EsbSidecar::default().health().await.is_none());
    }
}
