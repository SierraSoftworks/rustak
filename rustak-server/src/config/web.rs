//! `[web.public]` and `[web.marti]` — the two HTTP listeners.
//!
//! They are separate sections because they are separate trust boundaries, not
//! because they serve different routes. `[web.public]` is the one a browser and
//! a not-yet-enrolled device reach: it carries the admin UI, `/api/v1`,
//! `/oauth/*`, `/login/*` and the enrollment endpoints, so it needs a
//! certificate the *client* already trusts — from ACME, from files, or (the
//! LAN-only story) from our own CA with a trust-bootstrap package.
//! `[web.marti]` is the mutually authenticated one: every caller presents a
//! client certificate we issued, which is both its authentication and its
//! device identity.
//!
//! There is deliberately no plaintext mode without an explicit opt-in. A TAK
//! server hands out credentials and certificates; serving that over HTTP
//! because a `mode` key was left at a permissive default is not a mistake we
//! want to make available by accident.

use std::path::PathBuf;

use rustak_core::config::ListenAddr;
use serde::{Deserialize, Serialize};

/// The default public listen address: the TAK "webtak" port.
fn default_public_listen() -> Vec<ListenAddr> {
    vec![ListenAddr::new("", 8446)]
}

/// The default Marti mTLS listen address.
fn default_marti_listen() -> ListenAddr {
    ListenAddr::new("", 8443)
}

/// Whether the Marti listener is bound at all.
fn default_true() -> bool {
    true
}

/// How often `mode = "files"` looks at the pair on disk.
///
/// Thirty seconds is two `stat` calls a minute — nothing — and it is the
/// difference between a renewal being served within the minute and an
/// operator wondering why the certificate a sidecar wrote an hour ago is not
/// the one their browser sees.
fn default_reload_interval() -> chrono::Duration {
    chrono::Duration::seconds(30)
}

/// `[web]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// The browser- and enrollment-facing listener.
    #[serde(default)]
    pub public: PublicWebConfig,

    /// The mutually authenticated Marti listener.
    #[serde(default)]
    pub marti: MartiWebConfig,
}

/// `[web.public]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicWebConfig {
    /// The addresses to bind, all serving the same routes.
    ///
    /// Add `":443"` for browsers that will not be told a port number, and for
    /// ACME's `tls-alpn-01` challenge — binding it needs
    /// `CAP_NET_BIND_SERVICE` or a port-forwarding proxy.
    #[serde(default = "default_public_listen")]
    pub listen: Vec<ListenAddr>,

    /// An optional plaintext address, for ACME's `http-01` challenge and a
    /// permanent redirect to the HTTPS listener.
    ///
    /// It serves nothing else: it is not an insecure copy of the API, and
    /// `allow_insecure_http` is not what turns it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_bind: Option<ListenAddr>,

    /// Permits `[web.public.tls] mode = "none"`.
    ///
    /// Two keys rather than one, because "serve this API without TLS" should
    /// take a deliberate second statement. Development and the end-to-end
    /// suites set both; nothing else should.
    #[serde(default)]
    pub allow_insecure_http: bool,

    /// How this listener gets its certificate.
    #[serde(default)]
    pub tls: TlsConfig,
}

impl Default for PublicWebConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            listen: default_public_listen(),
            plain_bind: None,
            allow_insecure_http: false,
            tls: TlsConfig::default(),
        }
    }
}

impl PublicWebConfig {
    /// Reports whether any configured address binds the given port.
    ///
    /// ACME validation asks this: a certificate authority connects to port 443
    /// for `tls-alpn-01` and port 80 for `http-01`, and no other port will do.
    pub fn listens_on(&self, port: u16) -> bool {
        self.listen.iter().any(|address| address.port() == port)
    }
}

/// `[web.public.tls]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Where the public certificate comes from.
    #[serde(default)]
    pub mode: TlsMode,

    /// The full certificate chain, PEM encoded. Required by
    /// [`TlsMode::Files`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_file: Option<PathBuf>,

    /// The private key, PEM encoded. Required by [`TlsMode::Files`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,

    /// How often [`TlsMode::Files`] checks whether those two files changed.
    ///
    /// `"0"` switches the check off, which leaves
    /// `POST /api/v1/settings/tls/renew` as the way to pick a renewal up
    /// without a restart.
    #[serde(
        default = "default_reload_interval",
        with = "rustak_core::config::duration::humane"
    )]
    pub reload_interval: chrono::Duration,

    /// Whether a missing or unusable pair stops the server at start-up.
    ///
    /// `false`, because the deployment this mode is for renders the pair from
    /// a sidecar that starts *alongside* rustak: the listener binds with a
    /// certificate from this installation's own authority and swaps the files
    /// in when they appear. Set it when the files are baked into the image and
    /// their absence means something is wrong.
    #[serde(default)]
    pub require_files_at_start: bool,
}

impl Default for TlsConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            mode: TlsMode::default(),
            cert_file: None,
            key_file: None,
            reload_interval: default_reload_interval(),
            require_files_at_start: false,
        }
    }
}

impl TlsConfig {
    /// The first file an operator promised would be there and is not.
    ///
    /// `--check` asks this so that `require_files_at_start = true` fails where
    /// somebody can see it rather than at the next deploy. Nothing asks it of
    /// the default, which is a listener that waits for the files instead.
    pub fn missing_at_start(&self) -> Option<&std::path::Path> {
        if self.mode != TlsMode::Files || !self.require_files_at_start {
            return None;
        }

        [self.cert_file.as_deref(), self.key_file.as_deref()]
            .into_iter()
            .flatten()
            .find(|path| !path.is_file())
    }

    /// Refuses a `files` mode that promised its files and has not got them.
    ///
    /// Called by [`Config::validate`](super::Config::validate). The rule is
    /// here rather than in `validate.rs` because it is a fact about how these
    /// values are *used* — the default waits for a pair a sidecar has not
    /// written yet, so an absent file is evidence of nothing unless
    /// `require_files_at_start` says it is — and because the advice is most of
    /// the code.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming the first file that is not
    /// there.
    ///
    /// [`human_errors::Kind::User`]: human_errors::Kind::User
    pub(super) fn validate_files(&self) -> Result<(), human_errors::Error> {
        let Some(path) = self.missing_at_start() else {
            return Ok(());
        };

        Err(human_errors::user(
            format!(
                "`[web.public.tls] require_files_at_start` is set and {} is not there, so rustak would refuse to start.",
                path.display()
            ),
            &[
                "Write the certificate chain and its private key to those paths before starting rustak.",
                "Or leave `require_files_at_start = false`, which binds the listener with a certificate from rustak's own CA and swaps the files in as soon as they appear.",
            ],
        ))
    }

    /// How often the pair is re-checked, when anything checks it at all.
    pub fn reload_every(&self) -> Option<chrono::Duration> {
        (self.mode == TlsMode::Files && self.reload_interval > chrono::Duration::zero())
            .then_some(self.reload_interval)
    }
}

/// Where `[web.public]`'s certificate comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Our own CA issues it, and devices are given the CA in their enrollment
    /// package. The default: it needs no public DNS, no open port 80 and no
    /// second party, which is the LAN deployment this server is mostly for.
    /// Browsers will not trust it until the CA is installed.
    #[default]
    Internal,

    /// A certificate and key we read from disk, re-read while the server runs
    /// when they change — and waited for when they are not there yet.
    Files,

    /// A certificate obtained from an ACME certificate authority; see
    /// [`AcmeConfig`](super::AcmeConfig).
    Acme,

    /// No TLS at all, for development or behind a proxy that terminates it.
    /// Refused unless [`PublicWebConfig::allow_insecure_http`] is also set.
    None,
}

/// `[web.marti]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MartiWebConfig {
    /// Whether to bind the Marti listener.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// The address to bind.
    #[serde(default = "default_marti_listen")]
    pub listen: ListenAddr,

    /// What client certificate this listener demands.
    #[serde(default)]
    pub client_cert: ClientCertMode,
}

impl Default for MartiWebConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            enabled: true,
            listen: default_marti_listen(),
            client_cert: ClientCertMode::default(),
        }
    }
}

/// What a listener demands of a client certificate.
///
/// One variant today. TAK Server makes this optional on 8443 and accepts
/// bearer tokens there instead; rustak does not, because a device that cannot
/// present a certificate has not enrolled, and everything it might want on this
/// port is available on `[web.public]` with a bearer token. The enum stays so
/// that relaxing it later is a new variant rather than a change of type — and
/// so that `client_cert = "optional"` fails loudly rather than being accepted
/// and quietly ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientCertMode {
    /// The handshake fails without a certificate our CA issued.
    #[default]
    Required,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: WebConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, WebConfig::default());
        assert_eq!(parsed.public.listen, vec![ListenAddr::new("", 8446)]);
        assert_eq!(parsed.marti.listen, ListenAddr::new("", 8443));
        assert!(parsed.marti.enabled);
    }

    #[test]
    fn the_default_certificate_source_is_our_own_ca() {
        // The plan's reconciled decision: `internal` is the LAN-only story and
        // the default, so an installation that configures nothing still serves
        // TLS rather than plaintext.
        assert_eq!(TlsMode::default(), TlsMode::Internal);
        assert_eq!(WebConfig::default().public.tls.mode, TlsMode::Internal);
        assert!(!WebConfig::default().public.allow_insecure_http);
    }

    #[test]
    fn every_certificate_source_is_spelled_the_way_the_example_file_spells_it() {
        for (written, expected) in [
            ("internal", TlsMode::Internal),
            ("files", TlsMode::Files),
            ("acme", TlsMode::Acme),
            ("none", TlsMode::None),
        ] {
            let parsed: TlsConfig = toml::from_str(&format!("mode = \"{written}\"")).unwrap();

            assert_eq!(parsed.mode, expected, "{written}");
        }
    }

    #[test]
    fn an_optional_client_certificate_is_refused_by_name() {
        // TAK Server allows it; we do not. The failure has to name the value so
        // that somebody porting a TAK Server configuration is told, rather than
        // discovering that their mTLS port accepts anonymous callers.
        let Err(err) = toml::from_str::<MartiWebConfig>(r#"client_cert = "optional""#) else {
            panic!("`optional` should not be accepted");
        };

        assert!(err.to_string().contains("optional"), "{err}");
        assert!(err.to_string().contains("required"), "{err}");
    }

    #[test]
    fn a_file_pair_is_watched_every_thirty_seconds_and_waited_for_by_default() {
        // The deployment this is for: a sidecar renders the pair after the
        // task starts, so the default has to be "wait and pick it up", and the
        // waiting has to have something doing the looking.
        let parsed: TlsConfig = toml::from_str(r#"mode = "files""#).unwrap();

        assert_eq!(parsed.reload_interval, chrono::Duration::seconds(30));
        assert!(!parsed.require_files_at_start);
        assert_eq!(parsed.reload_every(), Some(chrono::Duration::seconds(30)));
        assert_eq!(parsed.missing_at_start(), None);
    }

    #[test]
    fn nothing_is_watched_in_the_other_modes_or_when_the_check_is_switched_off() {
        let off: TlsConfig = toml::from_str("mode = \"files\"\nreload_interval = \"0\"").unwrap();
        assert_eq!(off.reload_every(), None);

        let internal: TlsConfig = toml::from_str(r#"mode = "internal""#).unwrap();
        assert_eq!(internal.reload_every(), None);
    }

    #[test]
    fn a_promised_file_that_is_not_there_is_named() {
        let directory = tempfile::tempdir().unwrap();
        let present = directory.path().join("chain.pem");
        std::fs::write(&present, "unused").unwrap();

        let tls = TlsConfig {
            mode: TlsMode::Files,
            cert_file: Some(present.clone()),
            key_file: Some(directory.path().join("key.pem")),
            require_files_at_start: true,
            ..TlsConfig::default()
        };

        assert_eq!(
            tls.missing_at_start(),
            Some(directory.path().join("key.pem").as_path())
        );
        assert_eq!(
            TlsConfig {
                require_files_at_start: false,
                ..tls
            }
            .missing_at_start(),
            None,
            "the default waits for the files rather than refusing to start",
        );
    }

    #[test]
    fn a_listener_knows_which_ports_it_binds() {
        // ACME validation is the caller: only ports 80 and 443 satisfy a
        // challenge, whatever else is bound.
        let public: PublicWebConfig =
            toml::from_str(r#"listen = [":8446", "0.0.0.0:443"]"#).unwrap();

        assert!(public.listens_on(443));
        assert!(public.listens_on(8446));
        assert!(!public.listens_on(80));
    }

    #[test]
    fn a_misplaced_tls_key_is_reported_rather_than_ignored() {
        // `cert_file` belongs under [web.public.tls]; at the [web.public] level
        // it would otherwise be dropped, leaving the operator with a
        // self-signed certificate and no indication why.
        let Err(err) = toml::from_str::<PublicWebConfig>(r#"cert_file = "/etc/rustak/f.pem""#)
        else {
            panic!("an unknown key under [web.public] should be refused");
        };

        assert!(err.to_string().contains("cert_file"), "{err}");
    }
}
