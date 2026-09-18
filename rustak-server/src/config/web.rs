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
        }
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

    /// A certificate and key we read from disk, reloaded when they change.
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
