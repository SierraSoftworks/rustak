//! `rustak-plugin-example` — the copy-and-rename template for a rustak plugin.
//!
//! A plugin is a process that connects to a rustak server as a service identity
//! and does something useful with CoT. This one does the smallest thing that is
//! still a plugin: it logs a heartbeat on the configured interval and stops
//! cleanly when it is asked to. Everything else a real plugin does — publishing
//! CoT, subscribing to a channel, reporting to the control API — hangs off the
//! same three methods.
//!
//! Copy this crate to start a new plugin; `docs/plugins.md` has the recipe.

use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait, run};
use rustak_core::prelude::*;

/// What to say on each heartbeat, when the file does not say.
fn default_message() -> String {
    "The example sidecar is alive.".to_string()
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
}

impl Default for Settings {
    /// Written out rather than derived, because serde's `default = "..."`
    /// applies only when deserialising: a derived `Default` would hand back an
    /// empty message while an empty `[settings]` table gave the real one.
    fn default() -> Self {
        Self {
            message: default_message(),
        }
    }
}

/// The plugin itself: whatever it needs to remember between calls.
#[derive(Default)]
struct ExampleSidecar {
    /// Kept from [`Sidecar::start`], because the other methods are not given it.
    context: Option<SidecarContext<Settings>>,

    /// How many heartbeats this process has logged.
    heartbeats: u64,
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

        // M1: open the CoT stream here with `ctx.identity()`, and let the
        // harness deliver what arrives to `on_event`.
        self.context = Some(ctx);

        Ok(())
    }

    async fn tick(&mut self) -> Result<(), Error> {
        self.heartbeats += 1;

        let message = match &self.context {
            Some(context) => context.settings().message.as_str(),
            None => "The example sidecar is alive.",
        };

        info!(heartbeats = self.heartbeats, "{message}");

        Ok(())
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<(), Error> {
        // `SidecarEvent` is `#[non_exhaustive]`: the wildcard arm is what keeps
        // this plugin compiling as M1 and M6 add variants.
        match event {
            SidecarEvent::Connected { endpoint } => info!(%endpoint, "Connected to the stream."),
            SidecarEvent::Disconnected { reason } => {
                warn!(%reason, "The stream dropped; the harness will reconnect.");
            }
            SidecarEvent::Cot(message) => debug!(?message, "A CoT message arrived."),
            _ => debug!("An event this sidecar does not handle yet."),
        }

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Error> {
        info!(
            heartbeats = self.heartbeats,
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
    }

    #[test]
    fn an_absent_settings_table_gives_the_written_out_default() {
        let bare: SidecarConfig<Settings> =
            rustak_core::config::load_str("[service]\nname = \"example\"\n").unwrap();

        assert_eq!(bare.settings, Settings::default());
    }

    #[tokio::test]
    async fn the_plugin_heartbeats_between_starting_and_stopping() {
        let context =
            SidecarContext::from_config(config(), ExampleSidecar::VERSION, Shutdown::new())
                .unwrap();
        let mut sidecar = ExampleSidecar::default();

        sidecar.start(context).await.unwrap();
        sidecar.tick().await.unwrap();
        sidecar.tick().await.unwrap();
        sidecar
            .on_event(SidecarEvent::Disconnected {
                reason: "the server closed the connection".to_string(),
            })
            .await
            .unwrap();
        sidecar.stop().await.unwrap();

        assert_eq!(sidecar.heartbeats, 2);
    }
}
