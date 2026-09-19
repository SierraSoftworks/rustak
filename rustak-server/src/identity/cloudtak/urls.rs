//! The three base URLs CloudTAK stores for one server, and where they come
//! from.
//!
//! CloudTAK keeps `url`, `api` and `webtak` as three independent values and
//! makes no assumption that they share a host or a port (`compat/cloudtak.md`
//! §1). rustak happens to serve all three from one process, so the defaults
//! here are the one canonical host and the three configured listener ports —
//! but every part is overridable, because the common deployment is a container
//! whose ports are published as something else entirely and CloudTAK has to be
//! told the outside numbers rather than the inside ones.
//!
//! # Why the host is validated rather than trusted
//!
//! The value is echoed back into three URLs an operator pastes into another
//! system. A host carrying a scheme, a path, a credential or a space produces a
//! URL that either fails silently in CloudTAK or points somewhere else, so it
//! is refused here with a sentence saying what was wrong instead of being
//! interpolated and hoped for.

use rustak_api::{CloudTakPorts, CloudTakUrls};
use rustak_core::prelude::*;

use crate::config::Config;

/// The longest a host name may be, as DNS itself limits it.
const MAX_HOST: usize = 253;

/// The port `[web.public]` binds when nothing says otherwise, used only if an
/// installation has somehow been left with no public listen address at all.
const DEFAULT_PUBLIC_PORT: u16 = 8446;

/// The host used when the installation has not been told what it is called.
///
/// A CloudTAK on the same machine is the only deployment this is right for, and
/// it is what `config-packages` already falls back to — one wrong answer in two
/// places would be worse than one.
const FALLBACK_HOST: &str = "localhost";

/// Works out the three URLs, honouring whatever the caller overrode.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the host is not one that can be
/// put in a URL.
pub fn compose(
    config: &Config,
    host: Option<&str>,
    ports: Option<&CloudTakPorts>,
) -> Result<CloudTakUrls, Error> {
    let host = match host {
        Some(asked) => validate(asked)?,
        None => default_host(config),
    };

    let ports = ports.copied().unwrap_or_default();
    let stream = ports
        .stream
        .unwrap_or_else(|| config.stream.tls.listen.port());
    let marti = ports
        .marti
        .unwrap_or_else(|| config.web.marti.listen.port());
    let public = ports.public.unwrap_or_else(|| public_port(config));

    Ok(CloudTakUrls {
        stream: format!("ssl://{host}:{stream}"),
        api: format!("https://{host}:{marti}"),
        webtak: format!("https://{host}:{public}"),
    })
}

/// The name this installation calls itself by.
///
/// `[marti] public_host` first, because it already means "the name to put in a
/// URL a client will pass on"; then the configured base URL, which is what the
/// browser reaches us at; then the first domain; then loopback.
pub fn default_host(config: &Config) -> String {
    if let Some(host) = config.marti.public_host.as_deref() {
        return host.to_string();
    }

    if let Some(host) = config.server.base_url().as_deref().and_then(host_of) {
        return host;
    }

    config
        .server
        .canonical_domain()
        .unwrap_or(FALLBACK_HOST)
        .to_string()
}

/// The port the browser-facing listener is reached on.
///
/// The listener may bind several addresses — `:8446` plus `:443` is the
/// documented pair — and the first is the one an installation wrote down first,
/// which is the one it thinks of as its own. An operator publishing the other
/// overrides it in the request.
fn public_port(config: &Config) -> u16 {
    config
        .web
        .public
        .listen
        .first()
        .map_or(DEFAULT_PUBLIC_PORT, |address| address.port())
}

/// The authority of a base URL, without its scheme, port or path.
fn host_of(base: &str) -> Option<String> {
    let rest = base.split_once("://").map_or(base, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;

    // A bracketed IPv6 literal keeps its brackets and its colons; anything else
    // loses a trailing `:port`.
    let host = match authority.strip_prefix('[') {
        Some(_) => authority.split(']').next().map(|head| format!("{head}]"))?,
        None => authority.split(':').next()?.to_string(),
    };

    (!host.is_empty()).then_some(host)
}

/// Refuses a host that cannot be put in a URL unchanged.
fn validate(host: &str) -> Result<String, Error> {
    let host = host.trim();

    if host.is_empty() {
        return Err(human_errors::user(
            "A host name is needed for the URLs CloudTAK will be given.",
            &["Leave it out to use the name this installation calls itself by."],
        ));
    }

    if host.len() > MAX_HOST {
        return Err(human_errors::user(
            "That host name is too long to put in a URL.",
            &["A host name may be at most 253 characters."],
        ));
    }

    // The scheme and the port are ours to add, and a path, a query or a
    // credential in this field would produce a URL pointing somewhere else.
    let refused = ['/', '\\', '?', '#', '@', ' ', '\t', '"', '\'', '<', '>'];
    if host.contains(&refused[..]) || host.contains("://") {
        return Err(human_errors::user(
            "That host name has something in it that cannot go in a URL.",
            &[
                "Give the host name on its own — no scheme, no port, no path.",
                "Override the ports separately if this deployment publishes different ones.",
            ],
        ));
    }

    // A colon means a port, except inside the brackets of an IPv6 literal.
    let bracketed = host.starts_with('[') && host.ends_with(']');
    if host.contains(':') && !bracketed {
        return Err(human_errors::user(
            "That host name carries a port, which is set separately.",
            &["Give the host name on its own and put the port in 'ports'."],
        ));
    }

    Ok(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_core::config::ListenAddr;

    fn config() -> Config {
        let mut config = Config::default();

        config.server.domains = vec!["tak.example.com".to_string()];
        config.stream.tls.listen = ListenAddr::new("", 8089);
        config.web.marti.listen = ListenAddr::new("", 8443);
        config.web.public.listen = vec![ListenAddr::new("", 8446)];

        config
    }

    #[test]
    fn the_default_is_the_canonical_host_and_the_configured_ports() {
        let urls = compose(&config(), None, None).unwrap();

        assert_eq!(urls.stream, "ssl://tak.example.com:8089");
        assert_eq!(urls.api, "https://tak.example.com:8443");
        assert_eq!(urls.webtak, "https://tak.example.com:8446");
    }

    #[test]
    fn the_marti_public_host_wins_over_the_domain() {
        let mut config = config();
        config.marti.public_host = Some("tak.public".to_string());

        assert_eq!(default_host(&config), "tak.public");
    }

    #[test]
    fn a_configured_base_url_is_read_for_its_host_alone() {
        let mut config = config();
        config.server.base_url = Some("https://proxy.example.net:9443/tak/".to_string());

        assert_eq!(default_host(&config), "proxy.example.net");
    }

    #[test]
    fn an_ipv6_base_url_keeps_its_brackets() {
        assert_eq!(
            host_of("https://[2001:db8::1]:8446/"),
            Some("[2001:db8::1]".into())
        );
    }

    #[test]
    fn an_installation_with_no_name_falls_back_to_loopback() {
        let mut config = config();
        config.server.domains.clear();

        assert_eq!(default_host(&config), FALLBACK_HOST);
    }

    #[test]
    fn each_port_may_be_overridden_on_its_own() {
        // The deployment this feature was asked for: rustak inside a container,
        // published on three other ports.
        let ports = CloudTakPorts {
            stream: Some(28089),
            marti: Some(28443),
            public: Some(28446),
        };
        let urls = compose(&config(), Some("tak.example.com"), Some(&ports)).unwrap();

        assert_eq!(urls.stream, "ssl://tak.example.com:28089");
        assert_eq!(urls.api, "https://tak.example.com:28443");
        assert_eq!(urls.webtak, "https://tak.example.com:28446");
    }

    #[test]
    fn overriding_one_port_leaves_the_others_configured() {
        let ports = CloudTakPorts {
            stream: Some(28089),
            ..CloudTakPorts::default()
        };
        let urls = compose(&config(), None, Some(&ports)).unwrap();

        assert_eq!(urls.stream, "ssl://tak.example.com:28089");
        assert_eq!(urls.api, "https://tak.example.com:8443");
    }

    #[test]
    fn a_host_carrying_a_scheme_is_refused_rather_than_interpolated() {
        // Otherwise `ssl://https://host:8089`, which CloudTAK accepts and then
        // never connects with.
        for host in ["https://tak.example.com", "tak.example.com/marti", "a b"] {
            let err = compose(&config(), Some(host), None).unwrap_err();

            assert!(err.is(human_errors::Kind::User), "{host}: {err}");
        }
    }

    #[test]
    fn a_host_carrying_a_port_says_where_the_port_goes() {
        let err = compose(&config(), Some("tak.example.com:8443"), None).unwrap_err();

        assert!(err.description().contains("port"), "{err}");
    }

    #[test]
    fn an_ipv6_literal_is_accepted_with_its_brackets() {
        let urls = compose(&config(), Some("[2001:db8::1]"), None).unwrap();

        assert_eq!(urls.api, "https://[2001:db8::1]:8443");
    }

    #[test]
    fn an_empty_host_is_refused_rather_than_producing_a_schemeless_url() {
        let err = compose(&config(), Some("   "), None).unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
    }
}
