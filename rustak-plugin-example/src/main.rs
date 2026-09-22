//! `rustak-plugin-example` — the copy-and-rename template for a rustak plugin.
//!
//! A plugin is a process that connects to a rustak server as a service identity
//! and does something useful with CoT. This one does the smallest thing that is
//! still a plugin: it registers itself, appears on the map as a contact,
//! re-appears there after every reconnection, logs what arrives on the stream
//! and what the server tells it on the event feed, and stops cleanly when it is
//! asked to. Everything else a real plugin does — reading a feed, filtering a
//! channel, filing content into a mission — hangs off the same three methods.
//!
//! Registration and heartbeats are the harness's, not this file's: set
//! `[server] control` and they happen. What is here is the *reacting* half —
//! `SidecarEvent::Server`, which is the server-event feed — and the two hooks
//! that make this plugin configurable from the admin UI: [`Overrides`] is the
//! type that configuration is read into, its derived schema is what the form
//! there is drawn from, and `validate_config` is the last word on a candidate.
//!
//! Copy this crate to start a new plugin; `docs/plugins.md` has the recipe.

use std::time::Duration;

use rustak_client::control::ServerEventPayload;
use rustak_client::sidecar::{
    ConfigIssue, ConfigValidation, JsonSchema, ServiceSettings, Sidecar, SidecarContext,
    SidecarEvent, async_trait, parse_config, run, schema_for,
};
use rustak_core::prelude::*;
use rustak_cot::Event;
use rustak_cot::detail::contact::STREAMING_ENDPOINT;
use rustak_cot::detail::{Contact, Group};

/// The CoT type of a friendly ground unit, which is what puts a sidecar on the
/// map as a contact rather than as an unknown track.
const SA_TYPE: &str = "a-f-G-U-C";

/// How many ticks a position report stays fresh for. Three, so that one lost
/// heartbeat does not make this sidecar vanish from every map at once.
const SA_TICKS: u32 = 3;

/// What to say on each heartbeat, when the file does not say.
fn default_message() -> String {
    "The example sidecar is alive.".to_string()
}

/// What team this sidecar reports itself in, when the file does not say.
fn default_team() -> String {
    "Cyan".to_string()
}

/// What role this sidecar reports, when the file does not say.
fn default_role() -> String {
    "Team Member".to_string()
}

/// `[settings]` — this plugin's own configuration.
///
/// `deny_unknown_fields` is what makes `config.example.toml` a schema rather
/// than documentation that drifts: a misspelled key fails at start-up, naming
/// itself.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Settings {
    /// What each heartbeat says. Default: "The example sidecar is alive.".
    #[serde(default = "default_message")]
    message: String,

    /// Where this sidecar reports itself. Default: 0, 0.
    #[serde(default)]
    lat: f64,

    /// Where this sidecar reports itself. Default: 0, 0.
    #[serde(default)]
    lon: f64,

    /// The `<__group>` this sidecar joins. Default: "Cyan".
    #[serde(default = "default_team")]
    team: String,

    /// The role it reports within that group. Default: "Team Member".
    #[serde(default = "default_role")]
    role: String,
}

impl Default for Settings {
    /// Written out rather than derived, because serde's `default = "..."`
    /// applies only when deserialising: a derived `Default` would hand back an
    /// empty message while an empty `[settings]` table gave the real one.
    fn default() -> Self {
        Self {
            message: default_message(),
            lat: 0.0,
            lon: 0.0,
            team: default_team(),
            role: default_role(),
        }
    }
}

/// What an administrator may change from the admin UI while this is running.
///
/// Not `[settings]`: that table is the file this process was started with.
/// This is the document the *server* holds for the service, and deriving
/// `JsonSchema` on it is all it takes for the admin UI to draw a form instead
/// of a JSON text box — the doc comments below are that form's help text.
#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct Overrides {
    /// What each heartbeat says. Leave unset to use the message in the
    /// sidecar's own configuration file.
    #[serde(default)]
    #[schemars(length(max = 200))]
    message: Option<String>,
}

/// The plugin itself: whatever it needs to remember between calls.
#[derive(Default)]
struct ExampleSidecar {
    /// Kept from [`Sidecar::start`], because the other methods are not given it.
    context: Option<SidecarContext<Settings>>,

    /// How many heartbeats this process has logged.
    heartbeats: u64,

    /// The server's copy of this service's configuration, re-read on a cadence
    /// of its own, and what it last said.
    configured: ServiceSettings,
    overrides: Overrides,

    /// How many devices have joined the stream since this sidecar started,
    /// counted off the server-event feed rather than by watching for CoT.
    clients_seen: u64,
}

impl ExampleSidecar {
    /// This sidecar's situational-awareness event.
    ///
    /// The shape is the one a server reads a subscription's identity out of
    /// (`compat/streaming.md` §10): the service's own uid, a `<contact>` with
    /// the callsign and the streaming endpoint sentinel, and a `<__group>`.
    /// Without all three, a sidecar is a position with no name on it.
    fn position(&self) -> Vec<Event> {
        let Some(context) = &self.context else {
            return Vec::new();
        };

        let settings = context.settings();
        let stale = context
            .config()
            .sidecar
            .tick()
            .saturating_mul(SA_TICKS)
            .max(Duration::from_secs(60));

        vec![
            Event::builder(SA_TYPE, context.identity().uid().as_str())
                .how("m-g")
                .point(settings.lat, settings.lon)
                .stale_after(stale)
                .typed(
                    &Contact::new(context.descriptor().display()).with_endpoint(STREAMING_ENDPOINT),
                )
                .typed(&Group::new(settings.team.clone(), settings.role.clone()))
                .build(),
        ]
    }
}

#[async_trait]
impl Sidecar for ExampleSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        info!(
            uid = %ctx.identity().uid(),
            stream = ?ctx.config().server.stream,
            "The example sidecar is starting as {}.",
            ctx.descriptor().display(),
        );

        // The harness has already opened the stream by the time this is called;
        // everything that arrives on it comes back through `on_event`.
        self.context = Some(ctx);

        Ok(())
    }

    fn config_schema() -> Option<serde_json::Value> {
        Some(schema_for::<Overrides>())
    }

    async fn validate_config(&mut self, config: &serde_json::Value) -> ConfigValidation {
        // The schema has already been applied by the server; this is for what it
        // cannot say. A real plugin tries the API key against its upstream here.
        match parse_config::<Overrides>(config) {
            Ok(Overrides {
                message: Some(message),
            }) if message.trim().is_empty() => {
                ConfigIssue::at("/message", "A message has to say something.").into()
            }
            Ok(_) => ConfigValidation::accepted(),
            Err(refusal) => refusal,
        }
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        self.heartbeats += 1;

        // `Some` only when the server holds a document this has not applied yet.
        if let Some(context) = self.context.clone()
            && let Some(document) = self.configured.refresh(&context).await
        {
            match parse_config(&document) {
                Ok(overrides) => self.overrides = overrides,
                Err(refusal) => warn!(?refusal, "Ignoring a configuration this build cannot read."),
            }
        }

        let message = match (&self.overrides.message, &self.context) {
            (Some(message), _) => message.as_str(),
            (None, Some(context)) => context.settings().message.as_str(),
            (None, None) => "The example sidecar is alive.",
        };

        info!(heartbeats = self.heartbeats, "{message}");

        // Returned rather than sent: the harness owns the connection, and drops
        // this with a log line rather than queueing it if the stream is down.
        Ok(self.position())
    }

    // This plugin does not implement `Sidecar::health`, so the harness reports
    // `Heartbeat::healthy()` for it after every tick and the Services page shows
    // that. A plugin with something to say — a `degraded` state, a sentence, its
    // own counters — overrides it:
    //
    //     async fn health(&mut self) -> Option<rustak_api::Heartbeat> {
    //         Some(Heartbeat {
    //             state: ServiceState::Degraded,
    //             message: Some("The upstream has not answered for 4 minutes.".into()),
    //             metrics: serde_json::json!({ "events_published": 1204 }),
    //         })
    //     }
    //
    // Whatever it answers *is* the heartbeat, rather than something the
    // harness's own then overwrites; see `docs/plugins.md` -> "Saying more than
    // healthy".

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        // `SidecarEvent` is `#[non_exhaustive]`: the wildcard arm is what keeps
        // this plugin compiling as M2 and M6 add variants.
        match event {
            SidecarEvent::Connected { endpoint } => {
                info!(%endpoint, "Connected to the stream.");

                // A reconnection is a new subscription as far as the server is
                // concerned, so this is what gets the callsign back on the map.
                return Ok(self.position());
            }
            SidecarEvent::Negotiated { protobuf } => {
                info!(protobuf, "The stream settled on an encoding.");
            }
            SidecarEvent::Disconnected { reason } => {
                warn!(%reason, "The stream dropped; the harness will reconnect.");
            }
            SidecarEvent::Cot(event) => {
                info!(uid = %event.uid, r#type = %event.r#type, "A CoT event arrived.");
            }
            // The server-event feed: what happened on the server, rather than
            // what came down the CoT stream. A plugin that only publishes has no
            // use for it; one that reacts to who is online, to a mission
            // changing or to a package arriving does.
            SidecarEvent::Server(event) => match &event.payload {
                ServerEventPayload::ClientConnected(client) => {
                    self.clients_seen += 1;

                    info!(
                        username = %client.username,
                        uid = client.uid.as_deref().unwrap_or("(not yet said)"),
                        clients_seen = self.clients_seen,
                        "A client joined the stream.",
                    );
                }
                other => debug!(event = %event.name(), ?other, "The server said something."),
            },
            _ => debug!("An event this sidecar does not handle yet."),
        }

        Ok(Vec::new())
    }

    async fn stop(&mut self) -> Result<(), Error> {
        info!(
            heartbeats = self.heartbeats,
            clients_seen = self.clients_seen,
            "The example sidecar is stopping."
        );

        Ok(())
    }
}

#[tokio::main]
async fn main() {
    run::<ExampleSidecar>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::sidecar::SidecarConfig;

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    fn config() -> SidecarConfig<Settings> {
        rustak_core::config::load_str(EXAMPLE).expect("config.example.toml should load")
    }

    fn started() -> ExampleSidecar {
        let context =
            SidecarContext::from_config(config(), ExampleSidecar::VERSION, Shutdown::new())
                .expect("the example configuration describes a valid identity");

        ExampleSidecar {
            context: Some(context),
            ..ExampleSidecar::default()
        }
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        // The cheapest way to keep an example file honest: load it in a test,
        // so a key renamed in `Settings` and not here fails the build.
        let config = config();

        assert_eq!(config.service.name.as_str(), "example");
        assert_eq!(
            config
                .descriptor(ExampleSidecar::VERSION)
                .unwrap()
                .uid()
                .as_str(),
            "SERVICE-example"
        );
        assert_eq!(config.settings.message, "The example sidecar is alive.");
        assert_eq!(config.settings.team, "Cyan");
    }

    #[test]
    fn an_absent_settings_table_gives_the_written_out_default() {
        let bare: SidecarConfig<Settings> =
            rustak_core::config::load_str("[service]\nname = \"example\"\n").unwrap();

        assert_eq!(bare.settings, Settings::default());
    }

    #[test]
    fn the_heartbeat_is_an_sa_message_a_server_can_read_an_identity_out_of() {
        // uid, callsign, `contact/@endpoint` and `__group` are what a server
        // fixes a subscription's identity from; a heartbeat missing any of them
        // is a plugin that never appears on anybody's map.
        let sidecar = started();

        let published = sidecar.position();
        let event = published.first().expect("a started sidecar reports itself");

        assert_eq!(event.uid, "SERVICE-example");
        assert_eq!(event.r#type, SA_TYPE);
        assert_eq!(event.callsign(), Some("example"));
        assert_eq!(event.endpoint(), Some(STREAMING_ENDPOINT));
        assert_eq!(
            event.group().map(|group| group.name).as_deref(),
            Some("Cyan")
        );
        assert!(event.is_sa());
        assert!(!event.is_stale_at(event.time));
    }

    #[tokio::test]
    async fn a_candidate_configuration_is_held_to_more_than_its_schema() {
        let mut sidecar = started();

        for (config, valid) in [
            (serde_json::json!({}), true),
            (serde_json::json!({ "message": "Still here." }), true),
            (serde_json::json!({ "message": "   " }), false),
            (serde_json::json!({ "mesage": "Still here." }), false),
        ] {
            assert_eq!(
                sidecar.validate_config(&config).await.is_valid(),
                valid,
                "{config}"
            );
        }

        let schema = ExampleSidecar::config_schema().expect("this plugin has one");
        assert_eq!(schema["properties"]["message"]["maxLength"], 200);
    }

    #[tokio::test]
    async fn every_reconnection_republishes_the_contact() {
        // Without this the sidecar is on the map until the first outage and
        // then a uid with no name, because the server treats a reopened
        // connection as a new subscription.
        let mut sidecar = started();

        let republished = sidecar
            .on_event(SidecarEvent::Connected {
                endpoint: "ssl://tak.example.com:8089".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(republished.len(), 1);
        assert_eq!(republished[0].uid, "SERVICE-example");

        let quiet = sidecar
            .on_event(SidecarEvent::Disconnected {
                reason: "the server closed the connection".to_string(),
            })
            .await
            .unwrap();

        assert!(quiet.is_empty(), "there is nowhere to publish to");
    }

    #[tokio::test]
    async fn a_client_joining_the_stream_is_counted_off_the_server_event_feed() {
        // What `[server] control` buys a plugin: it hears that a device joined
        // without watching for its situational-awareness message, and without
        // polling `/Marti/api/clientEndPoints`.
        use rustak_client::control::{ClientEvent, ServerEvent, ServerEventPayload};

        let mut sidecar = started();

        let published = sidecar
            .on_event(SidecarEvent::Server(Box::new(ServerEvent {
                id: 1,
                at: "2026-09-18T12:00:00.000Z".parse().unwrap(),
                payload: ServerEventPayload::ClientConnected(ClientEvent {
                    username: "ada".into(),
                    uid: Some("ANDROID-1".into()),
                    callsign: Some("ADA".into()),
                }),
            })))
            .await
            .unwrap();

        assert_eq!(sidecar.clients_seen, 1);
        assert!(published.is_empty(), "noticing is not a reason to publish");
    }

    #[tokio::test]
    async fn the_plugin_heartbeats_between_starting_and_stopping() {
        let context =
            SidecarContext::from_config(config(), ExampleSidecar::VERSION, Shutdown::new())
                .unwrap();
        let mut sidecar = ExampleSidecar::default();

        sidecar.start(context).await.unwrap();
        let published = sidecar.tick().await.unwrap();
        sidecar.tick().await.unwrap();
        sidecar
            .on_event(SidecarEvent::Cot(Box::new(
                Event::builder("a-f-G-U-C", "ANDROID-1").build(),
            )))
            .await
            .unwrap();
        sidecar.stop().await.unwrap();

        assert_eq!(sidecar.heartbeats, 2);
        assert_eq!(
            published.len(),
            1,
            "every tick reports this sidecar's position"
        );
    }
}
