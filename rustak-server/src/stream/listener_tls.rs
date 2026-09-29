//! The `:8089` listener: TLS, a client certificate, and nothing else.
//!
//! There is no plaintext listener and no anonymous path. Every connection
//! completes a mutually authenticated handshake against this installation's own
//! authority, and the certificate it presents *is* the authentication — which
//! is why [`pki::Pki::stream_server_config`] builds the configuration with a
//! mandatory verifier and why there is no configuration key to soften it.
//!
//! # Why the handshake has its own timeout
//!
//! A TCP connection that never sends a ClientHello costs a task, a socket and a
//! slot in the connection limit for as long as it is left alone. That is the
//! cheapest denial of service there is against a TLS listener, and a timeout
//! around the handshake is the whole defence.
//!
//! # No ALPN, no SNI
//!
//! commoncommo — the library ATAK's streaming client is built on — offers
//! neither. A server advertising an ALPN list refuses a handshake that offers
//! none, so the stream's configuration deliberately has an empty one, and the
//! certificate resolver answers without a server name.
//!
//! # A vanished peer is the kernel's to notice
//!
//! Every accepted socket gets `TCP_NODELAY`, keepalive probes and — on Linux —
//! `TCP_USER_TIMEOUT`, sized from `idle_timeout` by [`PeerProbe`] so that a
//! phone that drops off the network is reclaimed in roughly `idle_timeout`
//! even while the server is still writing to it. The platform facts are in
//! [`peer_probe`](super::peer_probe). A socket that refuses an option is still
//! served: the options shorten how long a dead peer lingers, they are not what
//! makes a live one work. M10-02.
//!
//! [`pki::Pki::stream_server_config`]: crate::pki::Pki::stream_server_config

use std::net::SocketAddr;
use std::sync::Arc;

use rustak_core::config::ListenAddr;
use socket2::{SockRef, TcpKeepalive};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

use crate::pki::tls::from_tokio_rustls;
use crate::prelude::*;

use super::connection::{self, ConnDeps};
use super::metrics::StreamMetrics;
use super::peer_probe::PeerProbe;
use super::resolver::CertPrincipalResolver;

/// The bounds that belong to the listener rather than to one connection.
///
/// One argument rather than three, because they arrive together, they are all
/// read from the configuration at bind time, and a `run` that takes eight
/// positional values is one a caller gets wrong silently.
#[derive(Debug, Clone, Copy)]
pub struct ListenerLimits {
    /// How long a TCP connection has to complete its TLS handshake.
    pub handshake_timeout: std::time::Duration,
    /// How many connections may be open — or mid-handshake — at once.
    pub max_connections: usize,
    /// How long the connections still open at shutdown are given to close.
    pub drain: std::time::Duration,
}

/// A bound socket, before anything has been accepted on it.
#[derive(Debug)]
pub struct Bound {
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl Bound {
    /// The address the socket actually bound, which is what a test needs when
    /// it asked for port zero.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

/// Binds the stream listener.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error naming the address, because an address
/// already in use or a port that needs privileges is something an operator
/// fixes in the configuration.
#[instrument("stream.bind", skip_all, fields(listen = %listen), err(Display))]
pub async fn bind(listen: &ListenAddr) -> Result<Bound, Error> {
    let addresses = listen.to_socket_addrs()?;
    let address = *addresses.first().ok_or_else(|| {
        human_errors::user(
            format!("The stream listener address '{listen}' resolved to nothing."),
            &["Use an address like ':8089' or '0.0.0.0:8089'."],
        )
    })?;

    let listener = TcpListener::bind(address).await.wrap_user_err(
        format!("We could not bind the CoT stream listener to {address}."),
        &[
            "Check that nothing else is already listening on that address.",
            "Binding a port below 1024 needs CAP_NET_BIND_SERVICE or a forwarding proxy.",
        ],
    )?;

    let local_addr = listener.local_addr().or_system_err(&[
        "This is unexpected; please report it with the surrounding log entries.",
    ])?;

    info!(address = %local_addr, "The CoT stream listener is bound.");

    Ok(Bound {
        listener,
        local_addr,
    })
}

/// Accepts connections until `shutdown` is cancelled, then drains.
///
/// Returns `Ok(())` on a clean shutdown. An accept that fails is logged and the
/// loop carries on: a per-connection failure — a file-descriptor limit, a peer
/// that reset before the accept completed — is not a reason to stop serving
/// every other device.
///
/// [`ListenerLimits::drain`] bounds the wait for the connections that are still
/// open once the socket has stopped accepting. It is the same budget the HTTP
/// listeners get, because they are stopping at the same time and the process
/// has one deadline rather than three.
pub async fn run(
    bound: Bound,
    tls: Arc<rustls::ServerConfig>,
    resolver: Arc<dyn CertPrincipalResolver>,
    deps: ConnDeps,
    limits: ListenerLimits,
    shutdown: Shutdown,
) -> Result<(), Error> {
    let ListenerLimits {
        handshake_timeout,
        max_connections,
        drain,
    } = limits;
    let acceptor = TlsAcceptor::from(tls);
    let slots = Arc::new(Semaphore::new(max_connections.max(1)));
    let mut tuning = Tuning::new(PeerProbe::for_idle_timeout(deps.limits.idle_timeout));

    loop {
        let accepted = tokio::select! {
            biased;

            () = shutdown.cancelled() => break,
            accepted = accept(&bound.listener, &mut tuning) => accepted,
        };

        let (socket, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(err) => {
                warn!(error = %err, "Could not accept a stream connection.");
                continue;
            }
        };

        StreamMetrics::incr(&deps.metrics.accepted);

        // Acquired before the handshake, so a flood of connections that never
        // complete one cannot outnumber the devices that would.
        let Ok(slot) = Arc::clone(&slots).acquire_owned().await else {
            break;
        };

        let acceptor = acceptor.clone();
        let resolver = Arc::clone(&resolver);
        let deps = deps.clone();
        let shutdown = shutdown.clone();

        tokio::spawn(async move {
            serve_one(
                acceptor,
                resolver,
                deps,
                socket,
                peer,
                handshake_timeout,
                shutdown,
            )
            .await;

            drop(slot);
        });
    }

    drain_connections(&slots, max_connections, drain, &shutdown).await;

    info!("The CoT stream listener has stopped.");

    Ok(())
}

/// Accepts one connection and gives its socket the options every stream
/// socket carries.
///
/// The one accept path, so what the tests read back from a socket is what a
/// device's connection gets.
async fn accept(
    listener: &TcpListener,
    tuning: &mut Tuning,
) -> std::io::Result<(TcpStream, SocketAddr)> {
    let (socket, peer) = listener.accept().await?;
    tuning.apply(&socket);

    Ok((socket, peer))
}

/// The socket options one listener sets, and whether it has complained yet.
#[derive(Debug)]
struct Tuning {
    probe: PeerProbe,
    /// Set once an option has been refused: the platform will refuse it on
    /// every connection alike, and a line per device says nothing new.
    warned: bool,
}

impl Tuning {
    fn new(probe: PeerProbe) -> Self {
        Self {
            probe,
            warned: false,
        }
    }

    /// Sets `TCP_NODELAY` and the peer probe. Never refuses the connection.
    fn apply(&mut self, socket: &TcpStream) {
        // CoT messages are small and latency-sensitive; Nagle would hold a
        // position report back waiting for a second one seconds away.
        if let Err(err) = socket.set_nodelay(true) {
            debug!(error = %err, "Could not disable Nagle on a stream connection.");
        }

        if let Err((option, err)) = set_probe(socket, &self.probe)
            && self.first_failure()
        {
            warn!(
                option,
                error = %err,
                "Could not set a keepalive option on a stream connection; a device that drops off \
                 the network may stay connected past the idle timeout. Logged once per listener start.",
            );
        }
    }

    /// Whether this is the first refusal since the listener started.
    fn first_failure(&mut self) -> bool {
        !std::mem::replace(&mut self.warned, true)
    }
}

/// Sets keepalive and, where it exists, the user timeout; answers the first
/// option refused, having still tried the rest.
fn set_probe(socket: &TcpStream, probe: &PeerProbe) -> Result<(), (&'static str, std::io::Error)> {
    let socket = SockRef::from(socket);
    let mut refused = None;

    let keepalive = TcpKeepalive::new().with_time(probe.keepalive_idle);
    // The platforms where both the interval and the count can be set; see
    // `peer_probe` for what each does with them.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "windows",
    ))]
    let keepalive = keepalive
        .with_interval(probe.keepalive_interval)
        .with_retries(probe.keepalive_retries);

    if let Err(err) = socket.set_tcp_keepalive(&keepalive) {
        refused.get_or_insert(("SO_KEEPALIVE", err));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Err(err) = socket.set_tcp_user_timeout(Some(probe.user_timeout)) {
        refused.get_or_insert(("TCP_USER_TIMEOUT", err));
    }

    refused.map_or(Ok(()), Err)
}

/// Waits for the connections that were still open when the socket stopped.
///
/// Every connection task holds one permit for its whole life, so acquiring all
/// of them is the same question as "has everybody finished?" — and it asks it
/// without a second registry to keep in step with the first.
///
/// Giving up is not an error. A device on a flaky link that has not noticed the
/// `t-x-d-d` yet is the ordinary case, and the alternative to cutting it off is
/// a process that will not stop.
async fn drain_connections(
    slots: &Arc<Semaphore>,
    max_connections: usize,
    drain: std::time::Duration,
    shutdown: &Shutdown,
) {
    // `Semaphore::new` above took the same number, so it is one the semaphore
    // can hold; the conversion only has to not panic.
    let all = u32::try_from(max_connections.max(1)).unwrap_or(u32::MAX);

    let waited = tokio::select! {
        biased;

        // An operator who asks a second time is not asking us to keep waiting.
        () = shutdown.aborted() => false,
        acquired = tokio::time::timeout(drain, slots.acquire_many(all)) => acquired.is_ok(),
    };

    if !waited {
        warn!(
            ?drain,
            "Some stream connections were still open when the drain ended; they have been cut off."
        );
    }
}

/// What a failed handshake was, when it is something an operator should see.
///
/// Everything that reaches a listening socket fails a handshake sooner or
/// later — port scans, health checks, a browser pointed at the wrong port — so
/// the failures stay at `debug` by default. The two that do not are the two a
/// device suffers: it presented no certificate, or it presented one this
/// installation will not take. M2-15's field enrolment ended with ATAK
/// reporting `"Read error: ssl=…: Failure in SSL library"` — a TLS alert or a
/// closed connection, which is all a client sees of either — while the server
/// logged nothing at all above `debug`, so the refusal had to be guessed at
/// from the client's side. [`crate::pki::tls::client_verifier`] names the
/// certificate it turned away; this names the connection.
fn refusal(err: &std::io::Error) -> Option<&'static str> {
    match err.get_ref()?.downcast_ref::<rustls::Error>()? {
        rustls::Error::NoCertificatesPresented => Some(
            "the client presented no certificate, which a device that has not \
             finished enrolling will do",
        ),
        rustls::Error::InvalidCertificate(_) => {
            Some("the certificate the client presented was not one we accept")
        }
        _ => None,
    }
}

/// Completes one handshake, resolves the certificate, and serves.
async fn serve_one(
    acceptor: TlsAcceptor,
    resolver: Arc<dyn CertPrincipalResolver>,
    deps: ConnDeps,
    socket: TcpStream,
    peer: SocketAddr,
    handshake_timeout: std::time::Duration,
    shutdown: Shutdown,
) {
    let stream = match tokio::time::timeout(handshake_timeout, acceptor.accept(socket)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            StreamMetrics::incr(&deps.metrics.rejected);

            match refusal(&err) {
                Some(reason) => {
                    info!(%peer, reason, "A stream connection was refused at the handshake.")
                }
                None => debug!(%peer, error = %err, "A stream handshake failed."),
            }

            return;
        }
        Err(_) => {
            StreamMetrics::incr(&deps.metrics.rejected);
            debug!(%peer, "A stream handshake did not complete in time.");

            return;
        }
    };

    // The verifier has already refused anything that does not chain to us or
    // has been revoked, so a connection with no certificate here would mean the
    // listener was built with the wrong configuration entirely.
    let Some(certificate) = from_tokio_rustls(&stream) else {
        StreamMetrics::incr(&deps.metrics.rejected);
        warn!(%peer, "A stream connection completed the handshake without a client certificate.");

        return;
    };

    let identity = match resolver.resolve(&certificate, peer).await {
        Ok(identity) => identity,
        Err(err) => {
            StreamMetrics::incr(&deps.metrics.rejected);
            warn!(%peer, error = %err, "Refused a stream connection.");

            return;
        }
    };

    connection::run(deps, stream, identity, peer, shutdown).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_refusals_a_device_suffers_are_reported_and_the_noise_is_not() {
        // The field failure was invisible: rustak logged a refused handshake at
        // `debug`, so the only account of it was the client's "Failure in SSL
        // library" (M2-15). Both of these are now `info` with a reason.
        let no_certificate = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::NoCertificatesPresented,
        );
        let refused_certificate = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::Revoked),
        );

        assert!(refusal(&no_certificate).is_some_and(|reason| reason.contains("no certificate")));
        assert!(refusal(&refused_certificate).is_some());

        // A scanner, a `GET /` from a browser, a connection that went away:
        // ordinary noise on any listening socket, and not an operator's
        // problem.
        for noise in [
            std::io::Error::new(std::io::ErrorKind::InvalidData, rustls::Error::DecryptError),
            std::io::Error::from(std::io::ErrorKind::ConnectionReset),
        ] {
            assert_eq!(refusal(&noise), None, "{noise:?}");
        }
    }

    #[tokio::test]
    async fn an_accepted_socket_carries_the_probe_sized_from_the_idle_timeout() {
        // M10-02. Read back from a socket that came through the listener's own
        // accept path, so this is what a device's connection really gets.
        let bound = bind(&ListenAddr::new("127.0.0.1", 0)).await.unwrap();
        let probe = PeerProbe::for_idle_timeout(std::time::Duration::from_secs(90));
        let mut tuning = Tuning::new(probe);

        let (accepted, client) = tokio::join!(
            accept(&bound.listener, &mut tuning),
            TcpStream::connect(bound.local_addr()),
        );
        let (socket, _) = accepted.unwrap();
        let _client = client.unwrap();
        let options = SockRef::from(&socket);

        assert!(options.tcp_nodelay().unwrap());
        assert!(options.keepalive().unwrap());
        assert!(!tuning.warned, "nothing was refused on this platform");

        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
        ))]
        {
            assert_eq!(options.tcp_keepalive_time().unwrap(), probe.keepalive_idle);
            assert_eq!(
                options.tcp_keepalive_interval().unwrap(),
                probe.keepalive_interval
            );
            assert_eq!(
                options.tcp_keepalive_retries().unwrap(),
                probe.keepalive_retries
            );
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        assert_eq!(
            options.tcp_user_timeout().unwrap(),
            Some(probe.user_timeout)
        );
    }

    #[test]
    fn a_refused_option_is_reported_once_per_listener_not_per_connection() {
        let mut tuning = Tuning::new(PeerProbe::for_idle_timeout(std::time::Duration::ZERO));

        assert!(tuning.first_failure());
        assert!(!tuning.first_failure());
        assert!(!tuning.first_failure());
    }

    #[tokio::test]
    async fn binding_port_zero_reports_the_port_it_got() {
        let bound = bind(&ListenAddr::new("127.0.0.1", 0)).await.unwrap();

        assert_ne!(bound.local_addr().port(), 0);
        assert_eq!(bound.local_addr().ip().to_string(), "127.0.0.1");
    }

    #[tokio::test]
    async fn an_address_already_in_use_is_the_operators_to_fix() {
        let first = bind(&ListenAddr::new("127.0.0.1", 0)).await.unwrap();
        let taken = ListenAddr::new("127.0.0.1", first.local_addr().port());

        let Err(err) = bind(&taken).await else {
            panic!("binding a port somebody else holds should fail");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(
            err.to_string()
                .contains(&first.local_addr().port().to_string()),
            "the error names the port: {err}",
        );
    }
}
