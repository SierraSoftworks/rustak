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
        }
    }
}

impl ServerConfig {
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
