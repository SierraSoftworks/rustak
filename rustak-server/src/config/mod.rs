//! The server's configuration file.
//!
//! One TOML file describes an entire installation: who it is, what it listens
//! on, where its state lives, how it issues and accepts credentials, and how
//! long it keeps things. [`Config::load`] reads it through
//! [`rustak_core::config`] — which substitutes `${{ env.NAME }}` expressions
//! before parsing, so a secret never has to be written into the file — and then
//! runs [`Config::validate`].
//!
//! # `config.example.toml` is a test, not documentation
//!
//! Every struct here carries `#[serde(deny_unknown_fields)]`, which makes a
//! misspelled or misplaced key a load failure that names the key rather than a
//! setting that is silently ignored. The example file at the repository root
//! documents every key with its default and is **loaded by the test suite**, so
//! a key that is renamed here and not there fails the build.
//!
//! # Defaults are written out twice, on purpose
//!
//! serde's `#[serde(default = "...")]` applies only when deserialising, so a
//! derived `Default` would hand back empty strings and zero durations for the
//! same struct an empty file fills in properly. Every section therefore has a
//! hand-written `impl Default` that names the same functions, and every section
//! has a test asserting that the two agree.
//!
//! # Example
//!
//! ```
//! # fn main() -> Result<(), human_errors::Error> {
//! use rustak_server::config::{Config, TlsMode};
//!
//! // An installation that configures nothing still serves TLS, from rustak's
//! // own certificate authority.
//! let config = Config::default();
//! assert_eq!(config.web.public.tls.mode, TlsMode::Internal);
//! config.validate()?;
//! # Ok(())
//! # }
//! ```

pub mod acme;
pub mod auth;
pub mod marti;
pub mod oidc;
pub mod pki;
pub mod retention;
pub mod server;
pub mod storage;
pub mod stream;
mod validate;
pub mod web;

use std::path::PathBuf;

use human_errors::Error;
use serde::{Deserialize, Serialize};

pub use acme::{AcmeChallenge, AcmeConfig, AcmeDirectory};
pub use auth::{AuthConfig, OAuthClient, OAuthServerConfig, RateLimitConfig};
pub use marti::MartiConfig;
pub use oidc::OidcConfig;
pub use pki::{KeyType, PkiConfig};
pub use retention::RetentionConfig;
pub use server::{MAX_SHUTDOWN_TIMEOUT, ServerConfig};
pub use storage::StorageConfig;
pub use stream::{StreamConfig, StreamTlsConfig};
pub use web::{ClientCertMode, MartiWebConfig, PublicWebConfig, TlsConfig, TlsMode, WebConfig};

/// A complete rustak server configuration.
///
/// Every section defaults, so the smallest valid file is an empty one — which
/// gives an installation serving TLS from its own CA on `:8446`, Marti on
/// `:8443` and the CoT stream on `:8089`, with no way in until the first-run
/// setup wizard has been completed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Who this installation is and where it keeps its state.
    #[serde(default)]
    pub server: ServerConfig,

    /// The database, the content store and the stream segments.
    #[serde(default)]
    pub storage: StorageConfig,

    /// The public and Marti HTTP listeners.
    #[serde(default)]
    pub web: WebConfig,

    /// The TAK-compatible HTTP surface served on both of them.
    #[serde(default)]
    pub marti: MartiConfig,

    /// The CoT streaming listener.
    #[serde(default)]
    pub stream: StreamConfig,

    /// Tokens, credentials and who is allowed in.
    #[serde(default)]
    pub auth: AuthConfig,

    /// The internal certificate authority and what it issues.
    #[serde(default)]
    pub pki: PkiConfig,

    /// The public certificate, when it comes from an ACME authority.
    #[serde(default)]
    pub acme: AcmeConfig,

    /// How long we keep things.
    #[serde(default)]
    pub retention: RetentionConfig,
}

impl Config {
    /// Reads, interpolates, parses and validates a configuration file.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::User`] error when the file cannot be
    /// read, does not parse, contains a key we do not recognise, or describes a
    /// combination we could not carry out — see [`Config::validate`].
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let config: Self = rustak_core::config::load(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Parses and validates configuration text that is already in hand.
    ///
    /// This is what [`load`](Self::load) does once the file has been read, and
    /// what the example-file test and the integration suites use directly.
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load), minus the ways a file can fail to be read.
    pub fn load_str(contents: &str) -> Result<Self, Error> {
        let config: Self = rustak_core::config::load_str(contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Checks the rules that span more than one section.
    ///
    /// Called by [`load`](Self::load), and by `rustak --check`, which is the
    /// whole of what that flag does.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::User`] error naming the keys that
    /// disagree, and what to do about them.
    pub fn validate(&self) -> Result<(), Error> {
        validate::validate(self)
    }

    /// The SQLite database path, resolved against `[server] data_dir`.
    pub fn database_path(&self) -> PathBuf {
        self.storage.database(&self.server.data_dir)
    }

    /// The content-addressed blob directory, resolved against
    /// `[server] data_dir`.
    pub fn content_dir(&self) -> PathBuf {
        self.storage.content_dir(&self.server.data_dir)
    }

    /// The append-only stream segment root, resolved against
    /// `[server] data_dir`.
    pub fn streams_dir(&self) -> PathBuf {
        self.storage.streams_dir(&self.server.data_dir)
    }

    /// The first-run setup token file, resolved against `[server] data_dir`.
    pub fn setup_token_file(&self) -> PathBuf {
        self.auth.setup_token_file(&self.server.data_dir)
    }

    /// The `iss` claim our tokens carry: `[auth] issuer` when set, otherwise
    /// the server's base URL.
    ///
    /// [`None`] before the setup wizard has been completed, when we do not yet
    /// know what host we are reached on.
    pub fn issuer(&self) -> Option<String> {
        self.auth.issuer.clone().or_else(|| self.server.base_url())
    }

    /// A configuration for an in-process test.
    ///
    /// Every listener is on port 0 so that concurrent suites do not race for a
    /// fixed number, TLS is off (with the explicit opt-in that requires), and
    /// `user_acl` admits everybody so that a test can sign in without writing a
    /// filter. `admin_acl` is left denying, because administrator access comes
    /// from the `users.is_admin` column that the setup wizard sets — which is
    /// the path the tests should be exercising.
    ///
    /// `data_dir` is a parameter rather than a temporary directory made here
    /// because the caller has to hold the [`tempfile::TempDir`] for as long as
    /// the server runs; one created here would be deleted the moment this
    /// function returned.
    ///
    /// [`tempfile::TempDir`]: https://docs.rs/tempfile/latest/tempfile/struct.TempDir.html
    pub fn testing(data_dir: impl Into<PathBuf>) -> Self {
        let mut config = Self {
            server: ServerConfig {
                domains: vec!["localhost".to_string()],
                data_dir: data_dir.into(),
                ..ServerConfig::default()
            },
            ..Self::default()
        };

        config.web.public.listen = vec![rustak_core::config::ListenAddr::new("127.0.0.1", 0)];
        config.web.public.allow_insecure_http = true;
        config.web.public.tls.mode = TlsMode::None;
        config.web.marti.listen = rustak_core::config::ListenAddr::new("127.0.0.1", 0);
        config.stream.tls.listen = rustak_core::config::ListenAddr::new("127.0.0.1", 0);
        config.auth.user_acl = Some(
            // Provably infallible: `true` is a literal in the filter grammar.
            filt_rs::Filter::new("true").expect("the literal `true` filter is always valid"),
        );

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example file at the repository root, compiled into the test binary
    /// so that it cannot drift from the schema without failing the build.
    const EXAMPLE: &str = include_str!("../../../config.example.toml");

    #[test]
    fn the_documented_example_configuration_loads_and_validates() {
        // `deny_unknown_fields` makes the example a real test of the schema:
        // any key documented there that the server does not understand fails
        // here rather than being silently ignored at runtime. Validating it too
        // means the file we ship is one `rustak --check` accepts.
        let config = match Config::load_str(EXAMPLE) {
            Ok(config) => config,
            Err(err) => panic!("config.example.toml does not match the schema: {err}"),
        };

        assert_eq!(config.server.name, "rustak");
        assert_eq!(config.web.public.tls.mode, TlsMode::Internal);
    }

    /// A configuration with every optional key filled in, so that the
    /// example-file test below sees them all.
    fn fully_populated() -> Config {
        let mut config = Config::default();

        config.server.domains = vec!["tak.example.com".to_string()];
        config.server.base_url = Some("https://tak.example.com:8446".to_string());
        config.storage.database = Some(PathBuf::from("rustak.sqlite"));
        config.storage.content_dir = Some(PathBuf::from("content"));
        config.storage.streams_dir = Some(PathBuf::from("streams"));
        config.web.public.plain_bind = Some("0.0.0.0:80".parse().unwrap());
        config.web.public.tls.cert_file = Some(PathBuf::from("/etc/rustak/fullchain.pem"));
        config.web.public.tls.key_file = Some(PathBuf::from("/etc/rustak/privkey.pem"));
        config.marti.public_host = Some("tak.example.com".to_string());
        config.auth.issuer = Some("https://tak.example.com".to_string());
        config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
        config.auth.admin_acl = Some(filt_rs::Filter::new("true").unwrap());
        config.auth.setup_token_file = Some(PathBuf::from("setup-token"));
        config.auth.secret_key = Some("${{ env.RUSTAK_SECRET_KEY }}".to_string());
        config.auth.previous_secret_keys = vec!["old".to_string()];
        config.auth.oidc = Some(
            toml::from_str(
                r#"
                endpoint = "https://id.example.com"
                client_id = "rustak"
                client_secret = "shh"
                read_only_group = "observers"
                display_name = "Home SSO"
                "#,
            )
            .unwrap(),
        );
        config.pki.name_entries = vec![("OU".to_string(), "EUD".to_string())];
        config.pki.server_names = vec!["tak.lan".to_string()];
        config.pki.server_ips = vec!["192.168.1.10".parse().unwrap()];
        config.acme.contact = Some("ops@example.com".to_string());
        config.acme.domains = vec!["tak.example.com".to_string()];

        config
    }

    #[test]
    fn the_example_file_documents_every_key() {
        // The other half of the schema test: `deny_unknown_fields` catches a
        // key in the file that the server does not know, and this catches a key
        // the server knows that the file does not mention — including the
        // optional ones, which is why the fixture fills every `Option` in.
        let serialised =
            toml::to_string(&fully_populated()).expect("the configuration should serialise");

        for line in serialised.lines() {
            let Some((key, _)) = line.split_once(" = ") else {
                continue;
            };

            assert!(
                EXAMPLE.contains(&format!("{} =", key.trim())),
                "config.example.toml does not document `{}`",
                key.trim()
            );
        }
    }

    #[test]
    fn an_empty_file_is_the_written_out_default() {
        // The property every hand-written `impl Default` exists for, asserted
        // once over the whole tree.
        assert_eq!(Config::load_str("").unwrap(), Config::default());
    }

    #[test]
    fn a_misplaced_key_is_reported_rather_than_ignored() {
        // `admin_acl` belongs under [auth]; at the [server] level it would
        // otherwise be dropped, leaving the operator with the deny-by-default
        // ACL and no indication why.
        let Err(err) = Config::load_str("[server]\nadmin_acl = 'true'\n") else {
            panic!("a misplaced key should be refused");
        };

        assert!(err.to_string().contains("admin_acl"), "{err}");
    }

    #[test]
    fn a_plaintext_stream_listener_is_refused_by_name() {
        // The delta over the design documents: there is no plaintext CoT
        // stream and no anonymous access, and somebody porting a TAK Server
        // configuration has to be told rather than left believing 8087 is open.
        let Err(err) = Config::load_str("[stream.tcp]\nenabled = true\n") else {
            panic!("[stream.tcp] should be refused");
        };

        assert!(err.to_string().contains("tcp"), "{err}");
    }

    #[test]
    fn loading_validates_as_well_as_parses() {
        // A file that parses but could never start has to fail here, not at
        // the first request.
        let Err(err) = Config::load_str("[web.public.tls]\nmode = \"none\"\n") else {
            panic!("plaintext without the opt-in should be refused");
        };

        assert!(err.to_string().contains("plaintext"), "{err}");
    }

    #[test]
    fn a_missing_file_is_reported_with_its_path() {
        let Err(err) = Config::load("/rustak/definitely/not/here.toml") else {
            panic!("a missing file should be refused");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("not/here.toml"), "{err}");
    }

    #[test]
    fn a_secret_can_come_from_the_environment() {
        // The loader interpolates before parsing; an unset variable keeps its
        // marker so that whoever needs the secret can refuse it by name, rather
        // than the server starting with a blank key.
        let config =
            Config::load_str("[auth]\nsecret_key = \"${{ env.RUSTAK_CONFIG_UNSET_FOR_TEST }}\"\n")
                .unwrap();

        let secret = config.auth.secret_key.expect("the key should be present");
        assert!(rustak_core::config::env::is_unresolved(&secret));
    }

    #[test]
    fn the_paths_all_resolve_against_the_data_directory() {
        let config = Config::load_str("[server]\ndata_dir = \"/var/lib/rustak\"\n").unwrap();

        assert_eq!(
            config.database_path(),
            PathBuf::from("/var/lib/rustak/rustak.sqlite")
        );
        assert_eq!(
            config.content_dir(),
            PathBuf::from("/var/lib/rustak/content")
        );
        assert_eq!(
            config.streams_dir(),
            PathBuf::from("/var/lib/rustak/streams")
        );
        assert_eq!(
            config.setup_token_file(),
            PathBuf::from("/var/lib/rustak/setup-token")
        );
    }

    #[test]
    fn the_issuer_falls_back_to_the_servers_base_url() {
        let inferred = Config::load_str("[server]\ndomains = [\"tak.example.com\"]\n").unwrap();
        assert_eq!(
            inferred.issuer().as_deref(),
            Some("https://tak.example.com")
        );

        let configured = Config::load_str(
            "[server]\ndomains = [\"tak.example.com\"]\n[auth]\nissuer = \"https://tak.example.org\"\n",
        )
        .unwrap();
        assert_eq!(
            configured.issuer().as_deref(),
            Some("https://tak.example.org")
        );

        assert_eq!(Config::default().issuer(), None);
    }

    #[test]
    fn the_testing_configuration_is_one_a_test_can_actually_start() {
        let config = Config::testing("/tmp/rustak-test");

        config
            .validate()
            .expect("the testing config should validate");
        assert!(config.web.public.allow_insecure_http);
        assert_eq!(config.web.public.tls.mode, TlsMode::None);
        assert_eq!(config.server.data_dir, PathBuf::from("/tmp/rustak-test"));
        assert_eq!(
            config.database_path(),
            PathBuf::from("/tmp/rustak-test/rustak.sqlite")
        );
        assert_eq!(config.auth.user_acl().to_string(), "true");
        // Ephemeral ports, so that two suites running at once do not collide.
        assert!(config.web.public.listen.iter().all(|a| a.port() == 0));
    }
}
