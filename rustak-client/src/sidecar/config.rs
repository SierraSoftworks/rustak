//! A sidecar's configuration file.
//!
//! Every plugin reads the same three sections, whatever it does:
//!
//! | Section | What it says |
//! |---|---|
//! | `[service]` | Who this sidecar is: its name, and the credentials it connects with |
//! | `[server]` | Where the rustak server is |
//! | `[sidecar]` | How the harness runs it — the tick interval, the shutdown grace |
//!
//! and one it defines for itself:
//!
//! | `[settings]` | The plugin's own [`Sidecar::Settings`](super::Sidecar::Settings) |
//!
//! It is loaded by [`rustak_core::config`], so any value may be written as
//! `"${{ env.NAME }}"` and supplied from the environment instead of the file —
//! which is how a service token stays out of a file that gets committed or
//! attached to a support ticket.
//!
//! # `deny_unknown_fields` everywhere
//!
//! Every struct here refuses a key it does not recognise, so a misspelled or
//! misplaced setting is a start-up failure naming the key rather than a value
//! that silently does nothing. Put the same attribute on your own settings type;
//! `rustak-plugin-example` loads its own `config.example.toml` in a unit test,
//! which is the cheapest way to keep the example file honest.

use std::path::PathBuf;

use rustak_core::config::{duration, env};
use rustak_core::prelude::*;
use rustak_core::service::{Capability, ServiceDescriptor, ServiceEndpoints, ServiceIdentity};

/// Advice for a value whose `${{ env.NAME }}` expression was never substituted.
const ADVICE_UNRESOLVED: &[&str] = &[
    "Set the environment variable the expression names, or pass an environment file with --env.",
    "Remove the expression from the configuration file if the setting is not needed.",
];

/// Advice for a client certificate that is missing one of its two halves.
const ADVICE_HALF_A_CERTIFICATE: &[&str] = &[
    "Set both 'certificate' and 'key' under [service], or neither.",
    "A certificate without its private key cannot complete a TLS handshake.",
];

/// How often [`tick`](super::Sidecar::tick) runs when the file does not say.
fn default_tick() -> chrono::Duration {
    chrono::Duration::seconds(30)
}

/// How long [`stop`](super::Sidecar::stop) is given when the file does not say.
fn default_shutdown_grace() -> chrono::Duration {
    chrono::Duration::seconds(10)
}

/// A sidecar's whole configuration file.
///
/// `S` is the plugin's own settings type; it defaults to [`NoSettings`] so that
/// a plugin with nothing to configure does not have to name one.
#[derive(Debug, Deserialize)]
#[serde(
    deny_unknown_fields,
    bound(deserialize = "S: Deserialize<'de> + Default")
)]
pub struct SidecarConfig<S = NoSettings> {
    /// `[service]` — who this sidecar is.
    pub service: ServiceConfig,

    /// `[server]` — where the rustak server is.
    #[serde(default)]
    pub server: ServerConfig,

    /// `[sidecar]` — how the harness runs it.
    #[serde(default)]
    pub sidecar: HarnessConfig,

    /// `[settings]` — the plugin's own configuration.
    #[serde(default)]
    pub settings: S,
}

impl<S> SidecarConfig<S> {
    /// The identity this sidecar connects with.
    ///
    /// # Errors
    ///
    /// See [`ServiceConfig::identity`].
    pub fn identity(&self) -> Result<ServiceIdentity, Error> {
        self.service.identity()
    }

    /// What this sidecar publishes about itself, at the given version.
    ///
    /// Built from the identity's public half plus the descriptive fields that
    /// are not part of an identity at all — the version, the capabilities, and
    /// the endpoints it reached us on.
    ///
    /// # Errors
    ///
    /// See [`ServiceConfig::identity`] and [`ServerConfig::endpoints`].
    pub fn descriptor(&self, version: &str) -> Result<ServiceDescriptor, Error> {
        let mut descriptor = ServiceDescriptor::from(&self.identity()?);

        descriptor.display_name = self.service.display_name.clone();
        descriptor.version = Some(version.to_string());
        descriptor.capabilities = self.service.capabilities.clone();
        descriptor.endpoints = self.server.endpoints()?;

        Ok(descriptor)
    }
}

/// `[service]` — who this sidecar is.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// The service's name, which its `clientUid` (`SERVICE-<name>`), its
    /// control-API path and its certificate subject are all derived from.
    ///
    /// Required: everything else about a service is scoped to it, so there is
    /// no sensible default. One plugin binary deployed twice against the same
    /// server is two names.
    pub name: ServiceName,

    /// What to call this service in the admin UI. Default: its name.
    #[serde(default)]
    pub display_name: Option<String>,

    /// What this service advertises that it does, as lower-case dotted tokens
    /// such as `cot.publish`. Default: none.
    #[serde(default)]
    pub capabilities: Vec<Capability>,

    /// The token this service authenticates to `/api/v1/services/*` with.
    ///
    /// Write it as `"${{ env.RUSTAK_SERVICE_TOKEN }}"`: it is a credential, and
    /// the file is the part of a deployment that gets copied around.
    #[serde(default, deserialize_with = "optional_secret")]
    pub token: Option<Secret>,

    /// The client certificate this service opens the CoT stream with, in PEM.
    #[serde(default)]
    pub certificate: Option<PathBuf>,

    /// The private key belonging to `certificate`, in PEM.
    #[serde(default)]
    pub key: Option<PathBuf>,

    /// The truststore used to verify the server's certificate, in PEM. Default:
    /// the platform's own roots.
    #[serde(default)]
    pub truststore: Option<PathBuf>,
}

impl ServiceConfig {
    /// Assembles the [`ServiceIdentity`] this section describes.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::User`] error when the token still holds
    /// an unresolved `${{ env.NAME }}` expression — an unset credential is worth
    /// refusing rather than sending — or when exactly one half of the client
    /// certificate is configured, which fails at the TLS handshake with an
    /// error nobody can read.
    pub fn identity(&self) -> Result<ServiceIdentity, Error> {
        let mut identity = ServiceIdentity::new(self.name.clone());

        if let Some(token) = &self.token {
            refuse_unresolved("service.token", token.expose())?;
            identity = identity.with_credential(token.clone());
        }

        identity = match (&self.certificate, &self.key) {
            (Some(certificate), Some(key)) => identity.with_client_cert(certificate, key),
            (None, None) => identity,
            (certificate, _) => {
                let (set, missing) = match certificate {
                    Some(_) => ("certificate", "key"),
                    None => ("key", "certificate"),
                };

                return Err(human_errors::user(
                    format!(
                        "Your sidecar configuration sets [service] {set} without {missing}, and a client certificate needs both halves."
                    ),
                    ADVICE_HALF_A_CERTIFICATE,
                ));
            }
        };

        if let Some(truststore) = &self.truststore {
            identity = identity.with_truststore(truststore);
        }

        Ok(identity)
    }
}

/// `[server]` — where the rustak server is.
///
/// Every endpoint is optional because a plugin only needs the ones it uses, and
/// because M0 connects to none of them: an ADS-B feed that only publishes CoT
/// never calls Marti. They are strings rather than parsed URLs because the CoT
/// stream is addressed by a TAK connect string (`ssl://host:8089`), which M1's
/// stream client is what knows how to read.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// The CoT stream, e.g. `ssl://tak.example.com:8089`.
    #[serde(default)]
    pub stream: Option<String>,

    /// The Marti API, e.g. `https://tak.example.com:8443`.
    #[serde(default)]
    pub marti: Option<String>,

    /// The service control API, e.g. `https://tak.example.com:8446`.
    #[serde(default)]
    pub control: Option<String>,
}

impl ServerConfig {
    /// The endpoints as this sidecar reports them to the server.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::User`] error when an endpoint still holds
    /// an unresolved `${{ env.NAME }}` expression, which would otherwise be
    /// published to the admin UI as the address of a server nobody can reach.
    pub fn endpoints(&self) -> Result<ServiceEndpoints, Error> {
        for (key, value) in [
            ("server.stream", &self.stream),
            ("server.marti", &self.marti),
            ("server.control", &self.control),
        ] {
            if let Some(value) = value {
                refuse_unresolved(key, value)?;
            }
        }

        Ok(ServiceEndpoints {
            stream: self.stream.clone(),
            marti: self.marti.clone(),
            control: self.control.clone(),
        })
    }
}

/// `[sidecar]` — how the harness runs the plugin.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
    /// How often [`tick`](super::Sidecar::tick) is called. The first call
    /// happens as soon as the sidecar has started, not one interval later.
    #[serde(default = "default_tick", with = "duration::humane")]
    pub tick: chrono::Duration,

    /// How long [`stop`](super::Sidecar::stop) is given before the harness gives
    /// up waiting and reports the overrun as a bug.
    #[serde(default = "default_shutdown_grace", with = "duration::humane")]
    pub shutdown_grace: chrono::Duration,
}

impl Default for HarnessConfig {
    /// Written out rather than derived: serde's `default = "..."` applies only
    /// when deserialising, so a derived `Default` would hand back zero-length
    /// durations while an empty file gave 30 and 10 seconds.
    fn default() -> Self {
        Self {
            tick: default_tick(),
            shutdown_grace: default_shutdown_grace(),
        }
    }
}

impl HarnessConfig {
    /// The tick interval, as [`tokio::time::interval`] wants it.
    pub fn tick(&self) -> std::time::Duration {
        to_std(self.tick, default_tick())
    }

    /// The shutdown grace period, as [`rustak_core::runtime::with_grace`] wants
    /// it.
    pub fn shutdown_grace(&self) -> std::time::Duration {
        to_std(self.shutdown_grace, default_shutdown_grace())
    }
}

/// The settings of a plugin that has none.
///
/// `#[serde(deny_unknown_fields)]` on an empty table is deliberate: a plugin
/// that takes no settings should say so when handed some, rather than ignoring
/// a section its operator clearly meant to matter.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoSettings {}

/// Converts a configured span for the standard library, falling back rather than
/// panicking.
///
/// [`chrono::Duration::to_std`] fails only on a negative span, which
/// [`duration::humane`] already refuses at both ends — so the fallback is
/// unreachable through the configuration file, and is here because a sidecar
/// that will not start is worse than one that ticks at its default rate.
fn to_std(value: chrono::Duration, fallback: chrono::Duration) -> std::time::Duration {
    value
        .to_std()
        .or_else(|_| fallback.to_std())
        .unwrap_or(std::time::Duration::from_secs(30))
}

/// Reads an optional string into a [`Secret`], which redacts itself when logged.
fn optional_secret<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Secret>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.map(Secret::new))
}

/// Refuses a value whose `${{ env.NAME }}` expression was never substituted.
fn refuse_unresolved(key: &str, value: &str) -> Result<(), Error> {
    if env::is_unresolved(value) {
        return Err(human_errors::user(
            format!(
                "The [{key}] setting in your sidecar configuration still reads '{value}', which means the environment variable it names was not set."
            ),
            ADVICE_UNRESOLVED,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> SidecarConfig<NoSettings> {
        rustak_core::config::load_str(text).expect("the test configuration should load")
    }

    #[test]
    fn a_file_that_only_names_the_service_is_a_complete_configuration() {
        // The smallest thing a plugin author can put in front of somebody: a
        // name, and defaults for everything the harness needs.
        let config = load("[service]\nname = \"example\"\n");

        assert_eq!(config.service.name.as_str(), "example");
        assert_eq!(config.sidecar.tick(), std::time::Duration::from_secs(30));
        assert_eq!(
            config.sidecar.shutdown_grace(),
            std::time::Duration::from_secs(10),
        );
        assert_eq!(
            config.server.endpoints().unwrap(),
            ServiceEndpoints::default()
        );
    }

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        // The property the hand-written `Default` exists for: what serde gives
        // an unconfigured file and what `Default` gives a test must agree, or
        // the two disagree about how often a plugin ticks.
        let parsed: HarnessConfig = rustak_core::config::load_str("").unwrap();

        assert_eq!(parsed.tick, HarnessConfig::default().tick);
        assert_eq!(
            parsed.shutdown_grace,
            HarnessConfig::default().shutdown_grace,
        );
    }

    #[test]
    fn durations_are_written_the_way_a_person_writes_them() {
        let config = load(
            "[service]\nname = \"example\"\n\n[sidecar]\ntick = \"5m\"\nshutdown_grace = \"45s\"\n",
        );

        assert_eq!(config.sidecar.tick(), std::time::Duration::from_secs(300));
        assert_eq!(
            config.sidecar.shutdown_grace(),
            std::time::Duration::from_secs(45),
        );
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        // `deny_unknown_fields` is what makes an example file a schema. A
        // sidecar that silently ignored `tik` would tick every 30 seconds and
        // never say why.
        let Err(err) = rustak_core::config::load_str::<SidecarConfig<NoSettings>>(
            "[service]\nname = \"example\"\n\n[sidecar]\ntik = \"5m\"\n",
        ) else {
            panic!("a misspelled key should not load");
        };

        assert!(err.to_string().contains("tik"), "{err}");
    }

    #[test]
    fn a_service_name_is_validated_where_it_is_read() {
        let Err(err) = rustak_core::config::load_str::<SidecarConfig<NoSettings>>(
            "[service]\nname = \"Not A Service Name\"\n",
        ) else {
            panic!("an invalid service name should not load");
        };

        assert!(err.to_string().contains("service name"), "{err}");
    }

    #[test]
    fn an_unset_service_token_is_refused_by_name_rather_than_sent() {
        // `rustak_core::config::env` leaves the marker in place precisely so
        // that whoever needs the value can say what it was for. Sending the
        // literal `${{ env.… }}` text as a token would be an authentication
        // failure nobody could diagnose from the log line.
        let config = load(
            "[service]\nname = \"example\"\ntoken = \"${{ env.RUSTAK_SIDECAR_TOKEN_NOT_SET }}\"\n",
        );

        let Err(err) = config.identity() else {
            panic!("an unresolved token should not produce an identity");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("service.token"), "{err}");
        assert!(
            err.to_string().contains("RUSTAK_SIDECAR_TOKEN_NOT_SET"),
            "{err}",
        );
    }

    #[test]
    fn an_unset_endpoint_is_refused_before_it_is_published() {
        let config = load(
            "[service]\nname = \"example\"\n\n[server]\nstream = \"${{ env.RUSTAK_SIDECAR_STREAM_NOT_SET }}\"\n",
        );

        let Err(err) = config.server.endpoints() else {
            panic!("an unresolved endpoint should not be published");
        };

        assert!(err.to_string().contains("server.stream"), "{err}");
    }

    #[test]
    fn half_a_client_certificate_is_refused_before_the_handshake_fails() {
        // Without this, the failure surfaces as an opaque TLS error at connect
        // time; with it, the operator is told which of the two keys is missing.
        for (text, missing) in [
            ("certificate = \"/etc/rustak/example.pem\"", "key"),
            ("key = \"/etc/rustak/example.key\"", "certificate"),
        ] {
            let config = load(&format!("[service]\nname = \"example\"\n{text}\n"));

            let Err(err) = config.identity() else {
                panic!("half a client certificate should not produce an identity");
            };

            assert!(err.is(human_errors::Kind::User), "{err}");
            assert!(err.to_string().contains(missing), "{err}");
        }
    }

    #[test]
    fn both_halves_of_a_certificate_make_a_stream_capable_identity() {
        let config = load(
            "[service]\nname = \"example\"\ncertificate = \"/c.pem\"\nkey = \"/k.pem\"\ntruststore = \"/t.pem\"\n",
        );

        let identity = config.identity().unwrap();

        assert!(identity.has_client_cert());
        assert_eq!(identity.truststore(), Some(std::path::Path::new("/t.pem")));
    }

    #[test]
    fn a_descriptor_carries_what_is_published_and_nothing_else() {
        let config = load(
            r#"
            [service]
            name = "example"
            display_name = "Example sidecar"
            capabilities = ["cot.publish", "missions.subscribe"]
            token = "rsk_supersecret"
            certificate = "/etc/rustak/example.pem"
            key = "/etc/rustak/example.key"

            [server]
            stream = "ssl://tak.example.com:8089"
            control = "https://tak.example.com:8446"
            "#,
        );

        let descriptor = config.descriptor("1.2.3").unwrap();
        let published = serde_json::to_string(&descriptor).unwrap();

        assert_eq!(descriptor.display(), "Example sidecar");
        assert_eq!(descriptor.version.as_deref(), Some("1.2.3"));
        assert_eq!(descriptor.uid().as_str(), "SERVICE-example");
        assert_eq!(descriptor.capabilities.len(), 2);
        assert!(!published.contains("rsk_supersecret"), "{published}");
        assert!(!published.contains("example.key"), "{published}");
    }

    #[test]
    fn a_token_never_appears_in_a_debug_dump_of_the_configuration() {
        let config = load("[service]\nname = \"example\"\ntoken = \"rsk_supersecret\"\n");

        let printed = format!("{config:?}");
        assert!(!printed.contains("rsk_supersecret"), "{printed}");
        assert!(printed.contains("Secret(***)"), "{printed}");
    }

    #[test]
    fn a_plugin_can_define_its_own_settings_table() {
        #[derive(Debug, Default, Deserialize, PartialEq)]
        #[serde(deny_unknown_fields)]
        struct Settings {
            #[serde(default)]
            feed: Option<String>,
        }

        let config: SidecarConfig<Settings> = rustak_core::config::load_str(
            "[service]\nname = \"example\"\n\n[settings]\nfeed = \"https://example.com/feed\"\n",
        )
        .unwrap();

        assert_eq!(
            config.settings.feed.as_deref(),
            Some("https://example.com/feed")
        );

        // Absent, the plugin gets its own `Default` rather than a load failure.
        let bare: SidecarConfig<Settings> =
            rustak_core::config::load_str("[service]\nname = \"example\"\n").unwrap();
        assert_eq!(bare.settings, Settings::default());
    }
}
