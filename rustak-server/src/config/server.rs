//! `[server]` — who this installation is and where it keeps its state.
//!
//! Everything else in the file is a listener, a credential or a retention
//! horizon; this section is the handful of facts the rest of them are derived
//! from. `domains` in particular is load-bearing well beyond HTTP: the first
//! entry is the host written into enrollment QR codes, `.pref` files and the
//! issuer of our JWTs, so changing it invalidates what devices already hold.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The default installation name, shown to clients on `/Marti/api/version`.
fn default_name() -> String {
    "rustak".to_string()
}

/// The default state directory, relative to the working directory so that an
/// unconfigured `rustak` in a checkout keeps its files together rather than
/// scattering them.
fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}

/// The default drain budget: eight seconds.
///
/// Chosen against Docker rather than against anything inside rustak. `docker
/// stop` sends `SIGTERM`, waits ten seconds and then `SIGKILL`s, and the
/// checkpoint that makes a stopped installation's data directory tidy runs
/// *after* the drain — so a budget of ten would be spent at exactly the moment
/// the container was killed, and the checkpoint would never run. Eight leaves
/// the two seconds [`DATABASE_CLOSE_TIMEOUT`] needs inside the default grace.
///
/// [`DATABASE_CLOSE_TIMEOUT`]: crate::runtime::DATABASE_CLOSE_TIMEOUT
const DEFAULT_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// [`DEFAULT_SHUTDOWN_TIMEOUT`] as the configuration file spells a duration.
fn default_shutdown_timeout() -> chrono::Duration {
    // Provably infallible: eight seconds is inside every range chrono has.
    chrono::Duration::from_std(DEFAULT_SHUTDOWN_TIMEOUT)
        .expect("eight seconds is a duration chrono can hold")
}

/// How long an outbound request may take in total, by default.
///
/// reqwest imposes none of its own, and the failure it leaves open is the
/// common one rather than the exotic one: an identity provider whose load
/// balancer accepts the connection and never answers. Thirty seconds is longer
/// than any discovery document, JWKS or ACME exchange takes and short enough
/// that a hung provider does not hold an actix worker — and therefore an
/// administrator's sign-in — open indefinitely.
fn default_http_timeout() -> chrono::Duration {
    chrono::Duration::seconds(30)
}

/// How long the connect phase of an outbound request may take, by default.
///
/// Separate from the total because the two failures are different: a refused
/// or black-holed address should be given up on quickly, while a slow but live
/// response is worth waiting for.
fn default_http_connect_timeout() -> chrono::Duration {
    chrono::Duration::seconds(10)
}

/// The longest drain an operator may configure.
///
/// Not a technical limit: a minute is already far longer than any orchestrator
/// waits by default, and a budget beyond it would be one that only ever ends in
/// the `SIGKILL` it exists to avoid. Anybody who genuinely needs longer has to
/// raise their orchestrator's own timeout as well, and a refusal here is where
/// they find that out.
pub const MAX_SHUTDOWN_TIMEOUT: chrono::Duration = chrono::Duration::minutes(1);

/// `[server]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// The name clients see in the Marti version and config responses, and at
    /// the top of the admin UI.
    #[serde(default = "default_name")]
    pub name: String,

    /// The public host names this server answers to.
    ///
    /// The **first** entry is canonical: it is the host embedded in enrollment
    /// QR codes and device profiles, the subject of the internal server
    /// certificate, and the default JWT issuer. The rest are accepted aliases.
    ///
    /// Empty is allowed because the first-run setup wizard is how most
    /// installations set this; ACME refuses to start without it (see
    /// [`validate`](crate::config::Config::validate)).
    #[serde(default)]
    pub domains: Vec<String>,

    /// The externally visible base URL, when it cannot be inferred.
    ///
    /// Omitted, it is `https://<first domain>` — which is what an installation
    /// on a standard port wants. Set it when a proxy publishes rustak on a
    /// different port or path prefix than the one it binds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Whether `X-Forwarded-*` headers are believed.
    ///
    /// Off by default because a client can send those headers itself: trusting
    /// them without a proxy in front lets anyone claim any source address, which
    /// is exactly what the credential rate limiter keys on.
    #[serde(default)]
    pub trust_proxy: bool,

    /// Where the database, the secret key file, the content store, the
    /// append-only stream segments and the ACME cache live.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// How long connections that are still open are given to close on the way
    /// out, before they are cut off.
    ///
    /// The budget covers the drain and nothing else: the database checkpoint
    /// that follows it has its own short, fixed cap. Both have to fit inside
    /// whatever `docker stop`, systemd or Kubernetes allows before it sends
    /// `SIGKILL` — see `docs/deployment.md`.
    #[serde(
        default = "default_shutdown_timeout",
        with = "rustak_core::config::duration::humane"
    )]
    pub shutdown_timeout: chrono::Duration,

    /// How long an outbound HTTP request may take in total.
    ///
    /// Applies to every request the server makes: OIDC discovery and JWKS, the
    /// OIDC token exchange, ACME, and plugin webhooks. Zero turns the limit
    /// off, which is what a provider behind an exceptionally slow proxy needs
    /// and nothing else should.
    #[serde(
        default = "default_http_timeout",
        with = "rustak_core::config::duration::humane"
    )]
    pub http_timeout: chrono::Duration,

    /// How long the connect phase of an outbound HTTP request may take.
    #[serde(
        default = "default_http_connect_timeout",
        with = "rustak_core::config::duration::humane"
    )]
    pub http_connect_timeout: chrono::Duration,
}

impl Default for ServerConfig {
    /// Written out rather than derived: a derived `Default` would hand back an
    /// empty name and an empty path, disagreeing with what the same struct gets
    /// from an empty file. serde's `default = "..."` applies only when
    /// deserialising, so the two paths have to be spelled the same way twice.
    fn default() -> Self {
        Self {
            name: default_name(),
            domains: Vec::new(),
            base_url: None,
            trust_proxy: false,
            data_dir: default_data_dir(),
            shutdown_timeout: default_shutdown_timeout(),
            http_timeout: default_http_timeout(),
            http_connect_timeout: default_http_connect_timeout(),
        }
    }
}

impl ServerConfig {
    /// The outbound request budget, as the standard library spells a duration.
    ///
    /// [`None`] means an operator has asked for no limit at all.
    pub fn http_budget(&self) -> Option<std::time::Duration> {
        self.http_timeout
            .to_std()
            .ok()
            .filter(|budget| !budget.is_zero())
    }

    /// The outbound connect budget, as the standard library spells a duration.
    pub fn http_connect_budget(&self) -> Option<std::time::Duration> {
        self.http_connect_timeout
            .to_std()
            .ok()
            .filter(|budget| !budget.is_zero())
    }

    /// The drain budget, as the standard library spells a duration.
    ///
    /// A value the validator would have refused falls back to the default
    /// rather than saturating: this is reached from the shutdown path, where a
    /// panic or a zero-length wait would cost the checkpoint, and every caller
    /// has already been through [`validate`](crate::config::Config::validate).
    pub fn shutdown_budget(&self) -> std::time::Duration {
        self.shutdown_timeout
            .to_std()
            .ok()
            .filter(|budget| !budget.is_zero())
            .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT)
    }

    /// The budget actix is given, in the whole seconds its API takes.
    ///
    /// A second less than the budget, so that the runtime's own bounded wait is
    /// the one that reports a drain which overran — actix's timeout firing at
    /// the same instant would be a race over which of the two logged it.
    pub fn listener_drain_seconds(&self) -> u64 {
        self.shutdown_budget().as_secs().saturating_sub(1).max(1)
    }

    /// The canonical public host name, if one is configured.
    pub fn canonical_domain(&self) -> Option<&str> {
        self.domains.first().map(String::as_str)
    }

    /// The externally visible base URL, configured or inferred.
    ///
    /// Returns [`None`] when neither `base_url` nor `domains` is set — the
    /// first-run state, where the request's own `Host` header is all we have.
    pub fn base_url(&self) -> Option<String> {
        match (&self.base_url, self.canonical_domain()) {
            (Some(configured), _) => Some(configured.trim_end_matches('/').to_string()),
            (None, Some(domain)) => Some(format!("https://{domain}")),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        // The property the hand-written `Default` exists for: what serde gives
        // an unconfigured installation and what `Default` gives the wizard must
        // be the same thing, or the two disagree about where the database is.
        let parsed: ServerConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, ServerConfig::default());
        assert_eq!(parsed.name, "rustak");
        assert_eq!(parsed.data_dir, PathBuf::from("./data"));
    }

    #[test]
    fn the_drain_budget_leaves_room_for_the_checkpoint_inside_dockers_grace() {
        // Eight, not ten: `docker stop` waits ten seconds in total, and the
        // WAL truncation that follows the drain needs two of them.
        let parsed: ServerConfig = toml::from_str("").unwrap();

        assert_eq!(parsed.shutdown_timeout, chrono::Duration::seconds(8));
        assert_eq!(parsed.http_timeout, chrono::Duration::seconds(30));
        assert_eq!(parsed.http_connect_timeout, chrono::Duration::seconds(10));
        assert_eq!(parsed.shutdown_budget(), std::time::Duration::from_secs(8));
    }

    #[test]
    fn actix_is_given_a_second_less_than_the_budget() {
        // So that the runtime's own wait is the one which reports a drain that
        // overran, rather than the two racing to log it.
        let parsed: ServerConfig = toml::from_str(r#"shutdown_timeout = "30s""#).unwrap();

        assert_eq!(parsed.shutdown_budget(), std::time::Duration::from_secs(30));
        assert_eq!(parsed.listener_drain_seconds(), 29);

        // And never zero, whatever the budget is: actix reads a zero timeout as
        // "cut every connection off now".
        let parsed: ServerConfig = toml::from_str(r#"shutdown_timeout = "1s""#).unwrap();
        assert_eq!(parsed.listener_drain_seconds(), 1);
    }

    #[test]
    fn a_budget_the_validator_would_have_refused_falls_back_rather_than_vanishing() {
        // Reached from the shutdown path, where a zero-length wait would cost
        // the checkpoint this setting exists to protect.
        let broken = ServerConfig {
            shutdown_timeout: chrono::Duration::seconds(-1),
            ..ServerConfig::default()
        };

        assert_eq!(broken.shutdown_budget(), std::time::Duration::from_secs(8));
    }

    #[test]
    fn the_first_domain_is_the_canonical_one() {
        // Devices keep whichever host they enrolled against, so which entry is
        // canonical is a compatibility decision, not a cosmetic one.
        let parsed: ServerConfig =
            toml::from_str(r#"domains = ["tak.example.com", "tak.lan"]"#).unwrap();

        assert_eq!(parsed.canonical_domain(), Some("tak.example.com"));
        assert_eq!(
            parsed.base_url().as_deref(),
            Some("https://tak.example.com")
        );
    }

    #[test]
    fn a_configured_base_url_wins_over_the_inferred_one() {
        let parsed: ServerConfig = toml::from_str(
            r#"
            domains = ["tak.example.com"]
            base_url = "https://tak.example.com:8446/"
            "#,
        )
        .unwrap();

        // The trailing slash is dropped so that callers can concatenate paths
        // without producing `//api/v1`.
        assert_eq!(
            parsed.base_url().as_deref(),
            Some("https://tak.example.com:8446")
        );
    }

    #[test]
    fn a_first_run_installation_has_no_base_url_to_offer() {
        assert_eq!(ServerConfig::default().base_url(), None);
        assert_eq!(ServerConfig::default().canonical_domain(), None);
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<ServerConfig>(r#"domain = ["tak.example.com"]"#) else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("domain"), "{err}");
    }
}
