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

/// How a connection answers the TAK Protocol v1 negotiation.
///
/// **A compatibility-testing switch, not an operational one.** `accept` is what
/// an installation runs; the other two exist so that the EUD interop suite can
/// drive the two *negative* outcomes ATAK's own state machine implements, which
/// are otherwise only reachable against a broken server:
///
/// * `refuse` — answer the client's `t-x-takp-q` with `status="false"`. ATAK
///   logs `protocol negotiation request denied, using xml only` and stays in
///   XML for the life of the connection.
/// * `silent` — never send the `t-x-takp-v` offer at all. ATAK waits sixty
///   seconds, logs `Timed out waiting for protocol version support message`,
///   and carries on in XML **without reconnecting**.
///
/// Setting either on a real installation costs every client protobuf framing
/// and gains nothing, which is why the documentation in `config.example.toml`
/// says so in as many words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum NegotiationMode {
    /// Offer protobuf and accept a request for version 1. The only sane value.
    #[default]
    Accept = 0,

    /// Offer protobuf and then refuse the request.
    Refuse = 1,

    /// Never offer.
    Silent = 2,
}

/// `[stream]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamConfig {
    /// The mutually authenticated CoT stream listener.
    #[serde(default)]
    pub tls: StreamTlsConfig,

    /// What one connection may cost the server.
    #[serde(default)]
    pub limits: StreamLimits,

    /// How the server answers the protocol negotiation. See [`NegotiationMode`].
    ///
    /// Read once, where the listener builds its `ConnLimits`
    /// (`stream/mod.rs`), so two servers in one process each get their own.
    #[serde(default)]
    pub negotiation: NegotiationMode,
}

/// `[stream.limits]` — the bounds every connection is held to.
///
/// # Why a slow reader is disconnected rather than waited for
///
/// Every connection has its own bounded queue and the router never blocks on
/// one: a delivery that will not fit is dropped and counted. That alone keeps a
/// stalled device from back-pressuring everybody else, but a device that is
/// permanently behind would then receive an arbitrary subset of the traffic
/// forever, which looks to its operator like a server that is losing messages.
/// So after [`close_after_drops`](StreamLimits::close_after_drops) consecutive
/// drops the connection is closed and the client reconnects — which is the one
/// thing that actually resynchronises it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamLimits {
    /// The largest inbound message, in bytes, in either encoding.
    #[serde(default = "default_max_frame")]
    pub max_frame: usize,

    /// How many messages may be waiting to be written to one connection.
    #[serde(default = "default_queue_len")]
    pub queue_len: usize,

    /// How many deliveries in a row may be dropped before the connection is
    /// closed.
    #[serde(default = "default_close_after_drops")]
    pub close_after_drops: u64,

    /// How long the TLS handshake has to complete.
    #[serde(
        default = "default_handshake_timeout",
        with = "rustak_core::config::duration::humane"
    )]
    pub handshake_timeout: chrono::Duration,

    /// The most connections this listener accepts at once.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,

    /// Whether the server offers TAK Protocol v1 to clients that connect.
    #[serde(default = "default_enabled")]
    pub negotiate_protobuf: bool,

    /// Whether relayed messages are recorded to `cot_latest` and the history
    /// segments.
    #[serde(default = "default_enabled")]
    pub record_history: bool,
}

/// 8 MiB, matching `rustak_cot::codec::MAX_MESSAGE`.
fn default_max_frame() -> usize {
    rustak_cot::codec::MAX_MESSAGE
}

/// Deep enough to ride out a burst of position reports, shallow enough that a
/// stalled connection is noticed in seconds rather than minutes.
fn default_queue_len() -> usize {
    256
}

fn default_close_after_drops() -> u64 {
    512
}

fn default_handshake_timeout() -> chrono::Duration {
    chrono::Duration::seconds(10)
}

fn default_max_connections() -> usize {
    1024
}

impl Default for StreamLimits {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            max_frame: default_max_frame(),
            queue_len: default_queue_len(),
            close_after_drops: default_close_after_drops(),
            handshake_timeout: default_handshake_timeout(),
            max_connections: default_max_connections(),
            negotiate_protobuf: default_enabled(),
            record_history: default_enabled(),
        }
    }
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
    fn the_limits_have_defaults_a_deployment_never_has_to_write() {
        let parsed: StreamConfig = toml::from_str("").unwrap();

        assert_eq!(parsed.limits, StreamLimits::default());
        assert_eq!(parsed.limits.max_frame, rustak_cot::codec::MAX_MESSAGE);
        assert_eq!(parsed.limits.queue_len, 256);
        assert!(parsed.limits.negotiate_protobuf);
        assert!(parsed.limits.record_history);
    }

    #[test]
    fn a_deployment_can_turn_protobuf_negotiation_off() {
        // The escape hatch for a fleet whose clients mis-handle the offer: the
        // connection stays in XML, which every TAK client understands.
        let parsed: StreamConfig = toml::from_str(
            r#"
            [limits]
            negotiate_protobuf = false
            "#,
        )
        .unwrap();

        assert!(!parsed.limits.negotiate_protobuf);
        assert_eq!(parsed.limits.queue_len, 256, "the rest keep their defaults");
    }

    #[test]
    fn the_negotiation_switch_defaults_to_accepting() {
        let parsed: StreamConfig = toml::from_str("").unwrap();

        assert_eq!(
            parsed.negotiation,
            NegotiationMode::Accept,
            "an installation that does not mention the key gets the sane behaviour",
        );
    }

    #[test]
    fn the_negotiation_switch_is_parsed_into_the_value_the_listener_reads() {
        // The two values that exist for the EUD interop suite, and nothing else:
        // `refuse` makes the server answer `status="false"`, `silent` makes it
        // never offer at all. The parsed value is the whole story — nothing is
        // published process-wide — so these can run in any order.
        for (written, expected) in [
            ("refuse", NegotiationMode::Refuse),
            ("silent", NegotiationMode::Silent),
            ("accept", NegotiationMode::Accept),
        ] {
            let parsed: StreamConfig =
                toml::from_str(&format!(r#"negotiation = "{written}""#)).unwrap();

            assert_eq!(parsed.negotiation, expected);
        }
    }

    #[test]
    fn a_negotiation_mode_nobody_implements_is_refused_by_name() {
        let Err(err) = toml::from_str::<StreamConfig>(r#"negotiation = "ignore""#) else {
            panic!("an unknown negotiation mode should be refused");
        };

        assert!(err.to_string().contains("ignore"), "{err}");
    }

    #[test]
    fn an_idle_timeout_is_written_the_way_a_person_writes_one() {
        let parsed: StreamTlsConfig = toml::from_str(r#"idle_timeout = "5m""#).unwrap();

        assert_eq!(parsed.idle_timeout, chrono::Duration::minutes(5));
    }
}
