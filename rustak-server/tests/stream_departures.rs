//! How a client leaving the stream is named, end to end over TLS.
//!
//! M10-01. The first production roll of the disconnect line showed an
//! operator's routine restart of a sidecar as `reason=read_error`: the harness
//! dropped its socket without closing it, and the server called every
//! end-of-file without a TLS `close_notify` an error. Now the harness closes
//! its stream on shutdown (`client_closed`), and a client that simply stops
//! being there is `client_vanished` rather than `read_error`.
//!
//! # On timing
//!
//! Nothing here asserts an upper bound on anything. Every wait is for a state
//! change — a connection registered, a negotiation settled, a counter moved —
//! with a generous budget whose only job is to fail a hung wait.

mod stream_support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustak_client::sidecar::{
    NoSettings, Sidecar, SidecarConfig, SidecarContext, SidecarEvent, async_trait, drive,
};
use rustak_core::prelude::*;
use rustak_server::stream::{LeaveReason, StreamMetrics};
use tokio::sync::mpsc;

use stream_support::Harness;

/// How long a state change gets before the test calls it a hang.
const SETTLE: Duration = Duration::from_secs(30);

/// Waits for a counter to reach `want`, or says what it saw instead.
async fn await_count(metrics: &Arc<StreamMetrics>, reason: LeaveReason, want: u64) {
    let deadline = Instant::now() + SETTLE;

    while Instant::now() < deadline {
        if metrics.left.get(reason) >= want {
            return;
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!(
        "expected {want} disconnect(s) for {reason}, saw {:?}",
        metrics.left.nonzero().collect::<Vec<_>>(),
    );
}

/// Waits until the listener has `count` connections registered.
async fn await_connections(harness: &Harness, count: usize) {
    let deadline = Instant::now() + SETTLE;

    while Instant::now() < deadline {
        if harness.live.connected() == count {
            return;
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!(
        "expected {count} connection(s), saw {}",
        harness.live.connected()
    );
}

/// A sidecar with nothing to publish, reporting what its stream does.
#[derive(Default)]
struct Quiet {
    seen: Option<mpsc::UnboundedSender<SidecarEvent>>,
}

#[async_trait]
impl Sidecar for Quiet {
    const NAME: &'static str = "rustak-departures-test";
    const VERSION: &'static str = "0.0.0-test";
    type Settings = NoSettings;

    async fn start(&mut self, _: SidecarContext<NoSettings>) -> Result<(), Error> {
        Ok(())
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<rustak_cot::Event>, Error> {
        if let Some(seen) = &self.seen {
            let _ = seen.send(event);
        }

        Ok(Vec::new())
    }
}

/// Where `Harness::enroll` wrote a device's certificate, key and truststore.
fn material(harness: &Harness, username: &str, uid: &str) -> (String, String, String) {
    let dir = harness
        .data_dir
        .path()
        .join("clients")
        .join(username)
        .join(uid);
    let path = |file: &str| Path::new(&dir).join(file).display().to_string();

    (path("client.pem"), path("client.key"), path("ca.pem"))
}

#[tokio::test]
async fn a_sidecar_that_is_stopped_closes_its_stream_rather_than_vanishing() {
    // The production case: an operator restarts a sidecar. Its harness is
    // told to stop through its shutdown token, and the server has to record a
    // client that left — not one that failed, and not one that vanished.
    let harness = Harness::start().await;
    harness.enroll("svc.probe", "SERVICE-probe", &[]).await;
    let (certificate, key, truststore) = material(&harness, "svc.probe", "SERVICE-probe");

    let config: SidecarConfig<NoSettings> = rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "probe"
        certificate = "{certificate}"
        key = "{key}"
        truststore = "{truststore}"

        [server]
        stream = "ssl://{}"

        [sidecar]
        tick = "1h"
        shutdown_grace = "30s"
        "#,
        harness.addr,
    ))
    .expect("the sidecar configuration loads");

    let context = SidecarContext::from_config(config, Quiet::VERSION, Shutdown::new())
        .expect("a usable sidecar")
        .with_first_connect_hold(SETTLE);
    let shutdown = context.shutdown().clone();

    let (seen, mut events) = mpsc::unbounded_channel();
    let driving = tokio::spawn(async move {
        let mut sidecar = Quiet { seen: Some(seen) };

        drive(&mut sidecar, context).await
    });

    // Settled, so that nothing is still in flight in either direction when
    // the sidecar is stopped: the server's offer has been answered and the
    // answer read.
    tokio::time::timeout(SETTLE, async {
        while let Some(event) = events.recv().await {
            if matches!(event, SidecarEvent::Negotiated { .. }) {
                return;
            }
        }
    })
    .await
    .expect("the sidecar's stream negotiates");
    await_connections(&harness, 1).await;

    shutdown.cancel();
    driving
        .await
        .expect("the sidecar task joins")
        .expect("the sidecar stops without an error");

    let metrics = Arc::clone(harness.live.metrics());
    await_count(&metrics, LeaveReason::ClientClosed, 1).await;
    await_connections(&harness, 0).await;

    assert_eq!(metrics.left.get(LeaveReason::ClientClosed), 1);
    assert_eq!(
        metrics.left.get(LeaveReason::ClientVanished),
        0,
        "a sidecar that closes its stream has not vanished",
    );
    assert_eq!(
        metrics.left.get(LeaveReason::ReadError),
        0,
        "a routine restart is not an error",
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_client_that_drops_its_socket_without_closing_has_vanished() {
    // What every phone that loses signal or is killed looks like from here:
    // the TCP connection ends with no TLS `close_notify` before it.
    let harness = Harness::start().await;
    let alice = harness.enroll("alice", "UID-ALICE", &[]).await;

    let alpha = harness.eud(&alice, "ALPHA").await;
    await_connections(&harness, 1).await;

    drop(alpha);

    let metrics = Arc::clone(harness.live.metrics());
    await_count(&metrics, LeaveReason::ClientVanished, 1).await;
    await_connections(&harness, 0).await;

    assert_eq!(metrics.left.get(LeaveReason::ClientClosed), 0);
    assert_eq!(
        metrics.left.get(LeaveReason::ReadError),
        0,
        "a client that went away is not a fault",
    );

    harness.stop().await;
}
