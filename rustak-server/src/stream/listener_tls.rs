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
//! [`pki::Pki::stream_server_config`]: crate::pki::Pki::stream_server_config

use std::net::SocketAddr;
use std::sync::Arc;

use rustak_core::config::ListenAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

use crate::pki::tls::from_tokio_rustls;
use crate::prelude::*;

use super::connection::{self, ConnDeps};
use super::metrics::StreamMetrics;
use super::resolver::CertPrincipalResolver;

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

/// Accepts connections until `shutdown` is cancelled.
///
/// Returns `Ok(())` on a clean shutdown. An accept that fails is logged and the
/// loop carries on: a per-connection failure — a file-descriptor limit, a peer
/// that reset before the accept completed — is not a reason to stop serving
/// every other device.
pub async fn run(
    bound: Bound,
    tls: Arc<rustls::ServerConfig>,
    resolver: Arc<dyn CertPrincipalResolver>,
    deps: ConnDeps,
    handshake_timeout: std::time::Duration,
    max_connections: usize,
    shutdown: Shutdown,
) -> Result<(), Error> {
    let acceptor = TlsAcceptor::from(tls);
    let slots = Arc::new(Semaphore::new(max_connections.max(1)));

    loop {
        let accepted = tokio::select! {
            biased;

            () = shutdown.cancelled() => break,
            accepted = bound.listener.accept() => accepted,
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

        // CoT messages are small and latency-sensitive; Nagle would hold a
        // position report back waiting for a second one seconds away.
        if let Err(err) = socket.set_nodelay(true) {
            debug!(error = %err, "Could not disable Nagle on a stream connection.");
        }

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

    info!("The CoT stream listener has stopped.");

    Ok(())
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
            debug!(%peer, error = %err, "A stream handshake failed.");

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
