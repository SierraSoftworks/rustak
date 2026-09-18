//! The TAK stream client: connect, negotiate, stay connected.
//!
//! This is the client half of `compat/streaming.md` — the same `:8089` contract
//! `rustak-server` implements, seen from the EUD's side. A sidecar uses it to
//! publish and receive CoT; `rustak-server`'s integration tests use it (through
//! the `Eud` helper behind the `testing` feature) as a fake EUD, which is what
//! keeps the two halves honest about the same document.
//!
//! ```no_run
//! use futures::{SinkExt, StreamExt};
//! use rustak_client::stream::{Endpoint, StreamConfig, TlsIdentity, connect};
//!
//! # async fn example() -> Result<(), rustak_client::stream::StreamError> {
//! let config = StreamConfig::new("ssl://tak.example.com:8089".parse::<Endpoint>()?, "SERVICE-adsb")
//!     .with_tls(TlsIdentity::from_pem_files("ca.pem", "client.pem", "client.key")?);
//!
//! let mut stream = connect(&config).await?;
//! while let Some(event) = stream.next().await {
//!     let event = event?;
//!     tracing::info!(uid = %event.uid, "A message arrived.");
//! }
//! # Ok(()) }
//! ```
//!
//! # What the client does on its own
//!
//! | | |
//! |---|---|
//! | Negotiation | Answers the server's one `t-x-takp-v` offer, then switches both directions to protobuf on `t-x-takp-r status="true"` — see [`negotiation`](self::Negotiation) |
//! | Keepalive | Pings after 15 s of silence, repeats every 4.5 s, reports [`StreamError::RxTimeout`] at 25 s — ATAK's constants, see [`Keepalive`] |
//! | Framing | 8 MiB inbound cap, protobuf resync, one unreadable message never ends the connection |
//! | Reconnection | Only in [`Reconnecting`], which owns the backoff and the on-connect hook |
//!
//! # TLS only
//!
//! A default build dials TLS and nothing else. `tcp://…` connect strings are
//! refused at parse time unless the crate was built with `insecure-tcp` (which
//! `testing` implies), because rustak has no plaintext stream input and a
//! client that can be downgraded is a client that will be.

mod connect_string;
mod connection;
mod error;
mod keepalive;
mod negotiation;
mod reconnect;
pub mod tls;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::time::Duration;

pub use connect_string::Endpoint;
pub use connection::{AsyncIo, TakStream};
pub use error::{ADVICE_CONNECT_STRING, ADVICE_CONNECTIVITY, ADVICE_TLS_MATERIAL, StreamError};
pub use keepalive::Keepalive;
pub use negotiation::Negotiation;
pub use reconnect::{ConnectHook, MAX_BACKOFF, MIN_BACKOFF, Reconnecting};
pub use rustak_cot::codec::Mode;
pub use tls::TlsIdentity;

/// Everything a connection needs to know about itself.
///
/// Built with [`new`](StreamConfig::new) and adjusted with the `with_*`
/// methods; the fields are public because a test harness assembling one by
/// hand should not have to go through a builder.
#[derive(Debug)]
pub struct StreamConfig {
    /// Where the server is.
    pub endpoint: Endpoint,

    /// The client certificate and truststore. Required for a TLS endpoint.
    pub tls: Option<TlsIdentity>,

    /// This client's `clientUid` — what its pings are addressed from, and what
    /// the server keys its subscription on.
    pub uid: String,

    /// The callsign this client reports.
    ///
    /// The stream itself never sends an SA message — what a client says about
    /// itself is the caller's to decide — so this is carried for whoever
    /// builds one: a sidecar's position report, or the `Eud` test helper.
    pub callsign: Option<String>,

    /// Whether to accept a protobuf offer. `false` keeps the connection in XML
    /// the way CloudTAK does.
    pub negotiate: bool,

    /// When to ping, and when to give up.
    pub keepalive: Keepalive,

    /// How long to wait for the socket and the TLS handshake together.
    pub connect_timeout: Duration,

    /// Whether control messages are delivered to the caller as well as acted
    /// on. Off by default: a plugin has no use for a pong.
    pub pass_control: bool,
}

impl StreamConfig {
    /// How long the socket and handshake get before we give up, when the
    /// caller does not say.
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

    /// A connection to `endpoint`, identifying itself as `uid`.
    pub fn new(endpoint: Endpoint, uid: impl Into<String>) -> Self {
        Self {
            endpoint,
            tls: None,
            uid: uid.into(),
            callsign: None,
            negotiate: true,
            keepalive: Keepalive::ATAK,
            connect_timeout: Self::DEFAULT_CONNECT_TIMEOUT,
            pass_control: false,
        }
    }

    /// Attaches the client certificate and truststore.
    #[must_use]
    pub fn with_tls(mut self, identity: TlsIdentity) -> Self {
        self.tls = Some(identity);
        self
    }

    /// Sets the callsign this client reports.
    #[must_use]
    pub fn with_callsign(mut self, callsign: impl Into<String>) -> Self {
        self.callsign = Some(callsign.into());
        self
    }

    /// Turns protocol negotiation on or off.
    #[must_use]
    pub const fn with_negotiation(mut self, negotiate: bool) -> Self {
        self.negotiate = negotiate;
        self
    }

    /// Replaces the keepalive intervals.
    #[must_use]
    pub const fn with_keepalive(mut self, keepalive: Keepalive) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Replaces the connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Delivers control messages to the caller as well as acting on them.
    #[must_use]
    pub const fn with_pass_control(mut self, pass: bool) -> Self {
        self.pass_control = pass;
        self
    }
}

/// Opens a connection and hands back the stream, negotiation not yet begun.
///
/// The returned [`TakStream`] has completed its transport handshake and
/// nothing else: the server's offer arrives on the first poll, and the
/// negotiation exchange happens inside [`Stream::poll_next`](futures::Stream).
///
/// # Errors
///
/// [`StreamError::Identity`] when a TLS endpoint has no certificate material,
/// [`StreamError::Endpoint`] for a transport this build does not have,
/// [`StreamError::Timeout`] when the socket and handshake overrun
/// [`StreamConfig::connect_timeout`], and [`StreamError::Io`] for everything
/// the network does.
pub async fn connect(config: &StreamConfig) -> Result<TakStream, StreamError> {
    let io = tokio::time::timeout(config.connect_timeout, transport(config))
        .await
        .map_err(|_| StreamError::Timeout(format!("the connection to {}", config.endpoint)))??;

    tracing::debug!(endpoint = %config.endpoint, uid = %config.uid, "The TAK stream is up.");

    Ok(TakStream::new(io, config))
}

/// Dials the endpoint, in whichever transport it names.
async fn transport(config: &StreamConfig) -> Result<Box<dyn AsyncIo>, StreamError> {
    if config.endpoint.tls {
        let identity = config.tls.as_ref().ok_or_else(|| {
            StreamError::Identity(format!(
                "{} is a TLS endpoint and no client certificate was configured",
                config.endpoint,
            ))
        })?;

        let session = tls::connect(&config.endpoint, identity.client_config()?).await?;

        return Ok(Box::new(session));
    }

    plaintext(&config.endpoint).await
}

/// Opens a plaintext socket, in the builds that are allowed one.
#[cfg(any(test, feature = "insecure-tcp"))]
async fn plaintext(endpoint: &Endpoint) -> Result<Box<dyn AsyncIo>, StreamError> {
    tracing::warn!(%endpoint, "Connecting to a TAK stream without TLS; this is a test-only build.");

    Ok(Box::new(tls::dial(endpoint).await?))
}

/// Refuses a plaintext socket, in the builds that are not.
#[cfg(not(any(test, feature = "insecure-tcp")))]
async fn plaintext(endpoint: &Endpoint) -> Result<Box<dyn AsyncIo>, StreamError> {
    Err(StreamError::Endpoint(format!(
        "{endpoint}, because this build speaks TLS only",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_starts_out_behaving_like_atak() {
        // The defaults are the interoperable ones: negotiate, ping on ATAK's
        // clock, and hide the control traffic a plugin has no use for.
        let config = StreamConfig::new(Endpoint::tls("tak.example.com", 8089), "SERVICE-adsb");

        assert!(config.negotiate);
        assert!(!config.pass_control);
        assert_eq!(config.keepalive, Keepalive::ATAK);
        assert_eq!(
            config.connect_timeout,
            StreamConfig::DEFAULT_CONNECT_TIMEOUT
        );
        assert!(config.tls.is_none());
    }

    #[test]
    fn the_builders_say_what_they_change_and_nothing_else() {
        let config = StreamConfig::new(Endpoint::tls("tak.example.com", 8089), "SERVICE-adsb")
            .with_callsign("ADSB")
            .with_negotiation(false)
            .with_pass_control(true)
            .with_keepalive(Keepalive::OFF)
            .with_connect_timeout(Duration::from_secs(5));

        assert_eq!(config.callsign.as_deref(), Some("ADSB"));
        assert!(!config.negotiate);
        assert!(config.pass_control);
        assert!(config.keepalive.is_off());
        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.uid, "SERVICE-adsb");
    }

    #[tokio::test]
    async fn a_tls_endpoint_without_a_certificate_is_refused_before_the_socket_opens() {
        // Refusing here names the configuration; refusing during the handshake
        // names a TLS alert nobody can map back to a missing file.
        let config = StreamConfig::new(Endpoint::tls("127.0.0.1", 1), "SERVICE-adsb");

        let error = connect(&config)
            .await
            .expect_err("no certificate, no connection");

        assert!(matches!(error, StreamError::Identity(_)), "{error:?}");
        assert!(error.to_string().contains("client certificate"), "{error}");
    }

    #[tokio::test]
    async fn a_connect_timeout_names_the_endpoint_it_gave_up_on() {
        // 203.0.113.0/24 is TEST-NET-3: reserved for documentation, so the
        // connection hangs rather than being refused.
        let config = StreamConfig::new(Endpoint::new("203.0.113.1", 8089, false), "SERVICE-adsb")
            .with_connect_timeout(Duration::from_millis(50));

        let error = connect(&config)
            .await
            .expect_err("an unroutable host should time out");

        assert!(
            matches!(error, StreamError::Timeout(_) | StreamError::Io(_)),
            "{error:?}",
        );
    }
}
