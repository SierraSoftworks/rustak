//! When a connection is reclaimed, and what the line that says so reads.
//!
//! The suite exists for one production fault (M9-15): rustak closed the FIRMS
//! sidecar's stream every two to four minutes because the idle timer measured
//! *reads* only, and a sidecar that receives a busy channel and has nothing to
//! publish never sends a byte. Any receive-only TAK client on a busy server
//! had the same problem, which makes it a compatibility bug rather than a
//! sidecar one.
//!
//! # On timing
//!
//! Nothing here asserts an upper bound on work whose duration depends on the
//! host. The first test runs a relay loop until *its own* elapsed time is past
//! the idle timeout — a lower bound, which a slow host only makes safer — and
//! everything else waits on a state change with a generous budget whose only
//! job is to fail a hung wait rather than hang the suite.

mod stream_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustak_api::identity::Direction;
use rustak_client::stream::Keepalive;
use rustak_server::pki::RevokeReason;
use rustak_server::prelude::Services as _;
use rustak_server::stream::{LeaveReason, StreamMetrics};

use stream_support::{EXPECT, Harness};

const BOTH: Direction = Direction::Both;

/// Short enough that a test does not wait ninety seconds, long enough that a
/// host ten times slower than this one still relays well inside it.
const IDLE: Duration = Duration::from_secs(2);

/// How long a state change gets before the test calls it a hang.
const SETTLE: Duration = Duration::from_secs(20);

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

#[tokio::test]
async fn a_client_that_only_receives_is_never_reclaimed_for_saying_nothing() {
    // The production case, end to end: BRAVO is a sidecar with nothing to
    // publish (keepalive off, so the test controls every byte it sends, and
    // negotiation off, so it does not even answer the protocol offer). ALPHA
    // is the channel's traffic. Under the old read-idle rule BRAVO was dropped
    // `idle_timeout` after connecting, however much was being written to it.
    let harness = Harness::start_with(|config| {
        config.stream.tls.idle_timeout = chrono::Duration::from_std(IDLE).unwrap();
    })
    .await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;
    let bob = harness.enroll("bob", "UID-BOB", &[("blue", BOTH)]).await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    let mut bravo = harness
        .eud_with(&bob, "BRAVO", |config| {
            config
                .with_negotiation(false)
                .with_keepalive(Keepalive::OFF)
        })
        .await;
    await_connections(&harness, 2).await;

    let started = Instant::now();
    let mut relayed = 0;

    // Until well past the idle timeout — measured by the test's own clock, so
    // a slower host runs fewer rounds rather than failing.
    while started.elapsed() < IDLE * 2 {
        alpha.send_sa(51.5, -0.12).await.unwrap();
        bravo
            .expect(|event| event.uid == "UID-ALICE", EXPECT)
            .await
            .expect("a client being written to is still connected");
        relayed += 1;

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(relayed > 1, "the loop relayed {relayed} messages");
    assert_eq!(
        harness.live.connected(),
        2,
        "a client that has not sent a byte in {:?} is still here",
        started.elapsed(),
    );
    assert_eq!(
        harness.live.metrics().left.get(LeaveReason::Idle),
        0,
        "nothing was reclaimed for idleness",
    );

    harness.stop().await;
}

#[tokio::test]
async fn a_client_that_neither_sends_nor_receives_is_reclaimed_as_idle() {
    // What the timeout is actually for: a device that fell off the network
    // without closing its socket. Nothing is written to this one — it is the
    // only connection — and it sends nothing, so it is idle in both
    // directions and the disconnect line says so.
    let harness = Harness::start_with(|config| {
        config.stream.tls.idle_timeout = chrono::Duration::milliseconds(500);
    })
    .await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness
        .eud_with(&alice, "ALPHA", |config| {
            config
                .with_negotiation(false)
                .with_keepalive(Keepalive::OFF)
        })
        .await;
    await_connections(&harness, 1).await;

    let ended = alpha.expect(|_| true, SETTLE).await;
    assert!(
        ended.is_err(),
        "the connection should be reclaimed, not carry on: {ended:?}",
    );

    await_count(harness.live.metrics(), LeaveReason::Idle, 1).await;
    await_connections(&harness, 0).await;

    harness.stop().await;
}

#[tokio::test]
async fn a_client_that_closes_its_end_is_recorded_as_having_left() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    await_connections(&harness, 1).await;

    futures::SinkExt::close(alpha.stream_mut())
        .await
        .expect("the client closes its own end");

    await_count(harness.live.metrics(), LeaveReason::ClientClosed, 1).await;

    harness.stop().await;
}

#[tokio::test]
async fn a_revoked_certificate_names_itself_on_the_way_out() {
    // Every close from outside cancels the same token, so without a cause
    // recorded with it they all arrive as an unexplained disconnect.
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let mut alpha = harness.eud(&alice, "ALPHA").await;
    await_connections(&harness, 1).await;
    alpha.send_sa(51.5, -0.12).await.unwrap();

    harness
        .pki
        .revoke(
            harness.context.db(),
            &alice.fingerprint,
            RevokeReason::AdminAction,
            None,
        )
        .await
        .expect("the certificate is taken back");

    await_count(harness.live.metrics(), LeaveReason::Revoked, 1).await;

    harness.stop().await;
}

#[tokio::test]
async fn a_listener_that_is_draining_says_so_rather_than_blaming_the_client() {
    let harness = Harness::start().await;
    let alice = harness
        .enroll("alice", "UID-ALICE", &[("blue", BOTH)])
        .await;

    let _alpha = harness.eud(&alice, "ALPHA").await;
    await_connections(&harness, 1).await;

    let metrics = Arc::clone(harness.live.metrics());
    harness.stop().await;

    await_count(&metrics, LeaveReason::Shutdown, 1).await;
    assert_eq!(
        metrics.left.get(LeaveReason::Idle),
        0,
        "a drained connection is not an idle one",
    );
}
