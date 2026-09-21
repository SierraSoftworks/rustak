//! Waiting until a spawned listener is actually serving.
//!
//! Every integration suite that needs a real socket does the same three things:
//! bind an ephemeral port, hand the socket to actix, and spawn the server. The
//! trap is in what comes next. **Binding and serving are not the same moment.**
//! The socket is listening the instant it is bound, so the kernel completes a
//! client's TCP handshake from the backlog — but until the spawned `Server`
//! future has been polled and its workers have started, there is nobody to hand
//! that connection to, and actix drops it. A client dialling in that window does
//! not see a refused connection; it sees one that was accepted and then closed,
//! which `reqwest` reports as
//! `reqwest::Error { kind: Request, source: …(SendRequest, …) }` and `rustls` as
//! a connection closed mid-handshake.
//!
//! On an idle machine the window is too small to hit. Under a loaded
//! `cargo test --workspace` — and especially under `-Cinstrument-coverage` on a
//! two-vCPU runner — the spawned task is starved and the window is wide. This
//! has now cost four separate investigations in this repository: M6-01 and
//! M5-02 on `enroll_flows`, and `workload_identity` failing CI on
//! `8f52192`, which blocked an image publish.
//!
//! So the wait lives here rather than in each suite, where four of eight got it
//! wrong by omission.
//!
//! # Why the probe is below the protocol
//!
//! It asks nothing of the application: no route has to exist, no certificate has
//! to be issued, no row is written, and it works the same for plain HTTP and for
//! TLS. A listener with a worker behind it holds the connection open waiting for
//! the client to speak first, so **a read that times out is the ready signal**,
//! and an immediate EOF or reset is "not yet".

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;

/// How long to wait before giving up on a listener that never serves.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a single probe holds the connection open before calling it ready.
const PROBE: Duration = Duration::from_millis(250);

/// How long to wait between attempts.
const INTERVAL: Duration = Duration::from_millis(25);

/// Waits until `address` stops closing connections as they arrive.
///
/// Call it immediately after spawning a server and before the first request.
///
/// # Panics
///
/// If the listener has not started serving within [`READY_TIMEOUT`], which is a
/// server that failed to start rather than one that is slow.
pub async fn await_serving(address: SocketAddr) {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;

    loop {
        if let Ok(mut stream) = tokio::net::TcpStream::connect(address).await {
            let mut byte = [0u8; 1];

            match tokio::time::timeout(PROBE, stream.read(&mut byte)).await {
                // Held open, waiting for us to speak: a worker has it.
                Err(_elapsed) => return,
                // It spoke first, which it can only do if it is serving.
                Ok(Ok(1..)) => return,
                // EOF or reset: bound, but nothing is serving it yet.
                Ok(Ok(_) | Err(_)) => {}
            }
        }

        assert!(
            std::time::Instant::now() < deadline,
            "the listener on {address} never started serving",
        );

        tokio::time::sleep(INTERVAL).await;
    }
}
