//! `[stream.tls]` — the CoT streaming listener.
//!
//! # There is no `[stream.tcp]`
//!
//! TAK Server offers a plaintext streaming port where a device authenticates
//! with a `<auth>` element carrying a username and password in the clear, and
//! an anonymous mode where it does not authenticate at all. rustak offers
//! neither: the only way onto the stream is a client certificate this server
//! issued, which means enrolling first.
//!
//! The section is named here rather than silently absent because
//! `deny_unknown_fields` turns `[stream.tcp]` in a configuration file into an
//! error that names the key — which is what somebody migrating a TAK Server
//! deployment needs to see, instead of a plaintext port they believe is open
//! and is not.

use rustak_core::config::ListenAddr;
use serde::{Deserialize, Serialize};

/// The default CoT stream listen address.
fn default_stream_listen() -> ListenAddr {
    ListenAddr::new("", 8089)
}

/// How long a connection may go without traffic before it is closed.
///
/// TAK clients ping well inside this; the timeout is what reclaims a connection
/// whose device fell off the network without closing its socket.
fn default_idle_timeout() -> chrono::Duration {
    chrono::Duration::seconds(90)
}

/// `[stream]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamConfig {
    /// The mutually authenticated CoT stream listener.
    #[serde(default)]
    pub tls: StreamTlsConfig,
}

/// `[stream.tls]`.
///
/// There is no `client_cert` key: a certificate is always required here. See
/// the [module documentation](self).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamTlsConfig {
    /// Whether to bind the stream listener.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// The address to bind.
    #[serde(default = "default_stream_listen")]
    pub listen: ListenAddr,

    /// How long a connection may be idle before it is closed.
    #[serde(
        default = "default_idle_timeout",
        with = "rustak_core::config::duration::humane"
    )]
    pub idle_timeout: chrono::Duration,
}

/// The stream listener is bound unless an installation turns it off.
fn default_enabled() -> bool {
    true
}

impl Default for StreamTlsConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            listen: default_stream_listen(),
            idle_timeout: default_idle_timeout(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: StreamConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, StreamConfig::default());
        assert!(parsed.tls.enabled);
        assert_eq!(parsed.tls.listen, ListenAddr::new("", 8089));
        assert_eq!(parsed.tls.idle_timeout, chrono::Duration::seconds(90));
    }

    #[test]
    fn a_plaintext_stream_section_is_refused_by_name() {
        // The delta this module exists to enforce. Somebody porting a TAK
        // Server configuration must be told that there is no plaintext stream,
        // rather than left believing port 8087 is listening.
        let Err(err) = toml::from_str::<StreamConfig>(
            r#"
            [tcp]
            enabled = true
            listen = ":8087"
            "#,
        ) else {
            panic!("[stream.tcp] should be refused");
        };

        assert!(err.to_string().contains("tcp"), "{err}");
    }

    #[test]
    fn there_is_no_way_to_make_the_client_certificate_optional() {
        let Err(err) = toml::from_str::<StreamTlsConfig>(r#"client_cert = "optional""#) else {
            panic!("`client_cert` should not exist on the stream listener");
        };

        assert!(err.to_string().contains("client_cert"), "{err}");
    }

    #[test]
    fn an_idle_timeout_is_written_the_way_a_person_writes_one() {
        let parsed: StreamTlsConfig = toml::from_str(r#"idle_timeout = "5m""#).unwrap();

        assert_eq!(parsed.idle_timeout, chrono::Duration::minutes(5));
    }
}
