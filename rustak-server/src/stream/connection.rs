//! One accepted connection, from registration to disconnect notice.
//!
//! The order in [`run`] is the wire contract's order and none of it is
//! interchangeable (`compat/streaming.md` §4):
//!
//! 1. Register, so the connection can be addressed before it has said anything.
//! 2. Replay the latest position of every peer it may see — **plain XML**,
//!    because nothing has been negotiated yet.
//! 3. Send exactly one `t-x-takp-v`.
//! 4. Read until the client goes, the socket fails, or the connection goes
//!    quiet in **both** directions for longer than the idle timeout.
//! 5. Unregister, *then* tell the peers it has gone.
//!
//! Steps 2 and 3 both go through the per-connection writer queue, which is what
//! makes "replay before offer" true without a wait: one queue, one writer, one
//! order.
//!
//! # Idle is not the same as silent
//!
//! A TAK client pings after fifteen seconds of *inbound* silence and never
//! otherwise, so a receive-only client on a busy server — a sidecar with
//! nothing to publish, a screen somebody is watching — sends nothing at all
//! while it works perfectly. The idle timer therefore measures from
//! `max(last_rx, last_tx)` ([`Liveness`]): being written to counts as being
//! alive. What reclaims a peer that has *vanished* under outbound traffic is
//! `[stream.limits] write_timeout`, which fires when the socket stops taking
//! bytes. M9-15.
//!
//! # Every disconnect says why
//!
//! One `info` line per connection, carrying a [`LeaveReason`] and how long the
//! connection lasted. The reason is recorded by whichever task noticed first,
//! so a close from outside — a revoked certificate, a slow consumer, an
//! administrator — names itself rather than arriving as an unexplained EOF.
//!
//! # A bad message never costs the connection
//!
//! `TakCodec` consumes damage rather than reporting it, and a message that
//! frames but will not parse is counted and skipped. A TAK fleet contains
//! clients of every vintage, and a server that dropped a connection over one
//! malformed event would drop it again on the next update.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rustak_cot::codec::EncodedEvent;
use rustak_cot::codec::{Frame, Mode, TakCodec};
use rustak_cot::{CotTime, Event, proto, xml};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::config::stream::NegotiationMode;
use crate::prelude::*;

use super::liveness::{LeaveReason, Liveness};
use super::metrics::StreamMetrics;
use super::negotiation::{Intercepted, Negotiation};
use super::replay;
use super::resolver::StreamPrincipal;
use super::router::Router;
use super::subscription::{ConnHandle, ConnId, ConnStats, Outbound, Subscription};
use super::{notify, writer};

/// What one connection may cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnLimits {
    /// The largest inbound message.
    pub max_frame: usize,
    /// How deep this connection's outbound queue is.
    pub queue_len: usize,
    /// How many consecutive drops close it.
    pub close_after_drops: u64,
    /// How long it may go silent.
    pub idle_timeout: Duration,
    /// How long one write to it may take, and the budget its connect-time
    /// replay sends within.
    pub write_timeout: Duration,
    /// How it answers the TAK Protocol v1 negotiation.
    pub negotiate: NegotiationMode,
}

/// How long the writer is given to finish once the read side has ended.
///
/// Past this it is **aborted**, because a `timeout` around a `JoinHandle` stops
/// waiting without stopping anything, and the writer still here is one parked
/// on a socket whose peer has stopped reading. R-03 H1.
const WRITER_DRAIN: Duration = Duration::from_secs(5);

/// Everything a connection task shares with the rest of the listener.
#[derive(Clone, Debug)]
pub struct ConnDeps {
    /// The registry.
    pub router: Arc<Router>,
    /// The listener's counters.
    pub metrics: Arc<StreamMetrics>,
    /// What this server calls itself in a `t-x-takp-v`.
    pub server_version: String,
    /// Where an oversize message's pointer sends a client.
    pub public_url: Option<String>,
    /// The bounds.
    pub limits: ConnLimits,
}

/// Serves one connection until it ends.
#[instrument("stream.conn", skip_all, fields(conn, user = %identity.principal.username, peer = %peer))]
pub async fn run<IO>(
    deps: ConnDeps,
    io: IO,
    identity: StreamPrincipal,
    peer: SocketAddr,
    shutdown: Shutdown,
) where
    IO: AsyncRead + AsyncWrite + Send + 'static,
{
    let hub = Arc::clone(deps.router.hub());
    let id = hub.next_id();
    tracing::Span::current().record("conn", id.0);

    let (tx, rx) = mpsc::channel(deps.limits.queue_len.max(1));
    let stats = Arc::new(ConnStats::default());
    let closing = shutdown.child();
    let liveness = Arc::new(Liveness::new());
    let handle = ConnHandle::new(
        id,
        tx,
        Arc::clone(&stats),
        deps.limits.close_after_drops,
        closing.clone(),
    )
    .with_liveness(Arc::clone(&liveness));

    hub.register(
        Subscription::new(
            id,
            Arc::clone(&identity.principal),
            identity.groups.clone(),
            identity.fingerprint.clone(),
            peer,
            handle.clone(),
        )
        .with_device_id(identity.device_id)
        .with_incognito(identity.incognito),
    );
    StreamMetrics::incr(&deps.metrics.connected);

    let (read, write) = tokio::io::split(io);
    let sink = FramedWrite::new(
        write,
        TakCodec::with_limit(Mode::Xml, deps.limits.max_frame),
    );
    let mut writing = tokio::spawn(writer::run(
        rx,
        sink,
        writer::WriterContext {
            metrics: Arc::clone(&deps.metrics),
            public_url: deps.public_url.clone(),
            write_timeout: deps.limits.write_timeout,
            liveness: Arc::clone(&liveness),
        },
        closing.clone(),
    ));

    // Awaited, and awaited *after* the writer has been spawned, because it is
    // one message per connected peer into a queue that is shallower than the
    // fleet: it has to be able to wait for its own writer. See `replay`.
    replay::replay_latest_sa(&hub, id, deps.limits.write_timeout, &deps.metrics).await;

    let mut negotiation =
        Negotiation::with_mode(deps.limits.negotiate, deps.server_version.clone());
    if let Some(offer) = negotiation.offer(uuid::Uuid::new_v4().to_string(), CotTime::now()) {
        handle.send(Outbound::Event(Arc::new(EncodedEvent::new(offer))));
    }

    info!("A client is on the stream.");

    let mut framed = FramedRead::new(read, TakCodec::with_limit(Mode::Xml, deps.limits.max_frame));
    let ended = read_loop(&deps, &mut framed, &handle, &mut negotiation, id, &closing).await;

    // Whichever task noticed first names the cause; everything after it is a
    // consequence of that one.
    let reason = liveness.ended(ended);

    // Unregister first, so the notice is computed against the peers that are
    // left and is never delivered back to the connection it is about.
    let gone = hub.unregister(id);
    StreamMetrics::decr(&deps.metrics.connected);

    if let Some(gone) = gone {
        notify::on_disconnect(
            &hub,
            &gone,
            uuid::Uuid::new_v4().to_string(),
            CotTime::now(),
        );
    }

    // Both remaining senders, so the writer's queue closes and it can finish
    // whatever it had rather than being cut off mid-message.
    handle.close(reason);
    drop(handle);

    if tokio::time::timeout(WRITER_DRAIN, &mut writing)
        .await
        .is_err()
    {
        // The writer has had its drain and is still here, which means it is
        // parked inside a write on a socket its peer has stopped reading.
        // `tokio::io::split` closes the socket only when *both* halves drop,
        // so leaving the task alive leaks the descriptor, the TLS session and
        // the task itself for the life of the process. R-03 H1.
        writing.abort();
        let _ = writing.await;

        StreamMetrics::incr(&deps.metrics.writer_aborted);
        debug!("A stream connection's writer would not stop and was aborted.");
    }

    deps.metrics.left.incr(reason);

    info!(
        reason = %reason,
        connected_for = ?liveness.connected_for(),
        rx = stats.rx_msgs.load(std::sync::atomic::Ordering::Relaxed),
        tx = stats.tx_msgs.load(std::sync::atomic::Ordering::Relaxed),
        dropped = stats.dropped.load(std::sync::atomic::Ordering::Relaxed),
        "A client left the stream."
    );
}

/// Reads until the connection ends, routing everything it understands.
///
/// Answers why it stopped. The idle arm is a deadline rather than a timeout
/// around the read, because the connection is only idle when **neither**
/// direction has carried anything: a write that completes while this task is
/// parked moves the deadline, and the sleep that fires early re-arms against
/// the new one. See [`Liveness`].
async fn read_loop<R: AsyncRead + Unpin>(
    deps: &ConnDeps,
    framed: &mut FramedRead<R, TakCodec>,
    handle: &ConnHandle,
    negotiation: &mut Negotiation,
    id: ConnId,
    closing: &Shutdown,
) -> LeaveReason {
    let mut counters = Counters::default();
    let liveness = handle.liveness();
    let idle_timeout = deps.limits.idle_timeout;

    loop {
        let next = tokio::select! {
            biased;

            () = closing.cancelled() => {
                return liveness.reason().unwrap_or(LeaveReason::Shutdown);
            }
            () = tokio::time::sleep_until(liveness.idle_deadline(idle_timeout)) => {
                if !liveness.is_idle(idle_timeout) {
                    // Something was written while this task was parked, so the
                    // deadline has moved; wait for the new one.
                    continue;
                }

                debug!(
                    seconds = idle_timeout.as_secs(),
                    "A stream connection went quiet in both directions and was reclaimed.",
                );

                return LeaveReason::Idle;
            }
            next = framed.next() => next,
        };

        let frame = match next {
            None => return LeaveReason::ClientClosed,
            Some(Err(err)) => {
                debug!(error = %err, "A stream connection failed while reading.");

                return LeaveReason::ReadError;
            }
            Some(Ok(frame)) => frame,
        };

        // Whatever it is, the client is there. Counted before the message is
        // parsed, because a client sending nonsense is still a client.
        liveness.received();
        counters.sync(framed.decoder(), &deps.metrics);

        let Some(event) = decode(frame, &deps.metrics) else {
            continue;
        };

        StreamMetrics::incr(&deps.metrics.rx_msgs);
        handle
            .stats()
            .rx_msgs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if let Some(intercepted) = negotiation.on_event(&event, CotTime::now()) {
            answer(handle, framed, intercepted);
            continue;
        }

        deps.router.handle_inbound(id, event).await;
    }
}

/// Writes the negotiation answer and switches the reader with it.
///
/// The *reader* is switched here rather than in the writer because the client
/// sends nothing between its request and our answer — so the moment the answer
/// is queued, the next bytes to arrive will be protobuf.
fn answer<R: AsyncRead + Unpin>(
    handle: &ConnHandle,
    framed: &mut FramedRead<R, TakCodec>,
    intercepted: Intercepted,
) {
    match intercepted {
        Intercepted::Accepted(response) => {
            handle.send(Outbound::SwitchToProto(Arc::new(EncodedEvent::new(
                *response,
            ))));
            framed.decoder_mut().set_mode(Mode::Proto);

            debug!("A client negotiated TAK Protocol v1.");
        }
        Intercepted::Refused(response) => {
            handle.send(Outbound::Event(Arc::new(EncodedEvent::new(*response))));
        }
        Intercepted::Silent => {}
    }
}

/// Turns a frame into an event, counting what will not read.
fn decode(frame: Frame, metrics: &StreamMetrics) -> Option<Event> {
    match &frame {
        Frame::Xml(bytes) => match xml::parse(bytes) {
            Ok(event) => Some(event),
            Err(err) => unreadable(metrics, &err),
        },
        Frame::Proto(payload) => match proto::decode(payload) {
            Ok(message) => match proto::message_to_event(message) {
                Ok(event) => Some(event),
                // A frame carrying only `takControl` is not a message: the
                // client is saying something about the protocol, not the world.
                Err(rustak_cot::ConvertError::NoCotEvent) => None,
                Err(err) => unreadable(metrics, &err),
            },
            Err(err) => unreadable(metrics, &err),
        },
    }
}

/// Counts a message this server could not read, and drops it.
fn unreadable(metrics: &StreamMetrics, error: &dyn std::fmt::Display) -> Option<Event> {
    StreamMetrics::incr(&metrics.dropped_parse);
    debug!(%error, "Dropped a message this server could not read.");

    None
}

/// The codec's own counters, as deltas onto the listener's.
///
/// `TakCodec` never reports a framing failure — one unframeable message must
/// not cost a connection — so what it discarded is only visible as these
/// counters, and a listener that did not read them would show a clean stream
/// while dropping messages.
#[derive(Debug, Default)]
struct Counters {
    dropped: u64,
    skipped: u64,
}

impl Counters {
    /// Adds whatever the codec has discarded since the last frame.
    fn sync(&mut self, codec: &TakCodec, metrics: &StreamMetrics) {
        let dropped = codec.dropped();
        let skipped = codec.skipped();

        StreamMetrics::add(&metrics.dropped_parse, dropped.saturating_sub(self.dropped));
        StreamMetrics::add(&metrics.proto_resyncs, skipped.saturating_sub(self.skipped));

        self.dropped = dropped;
        self.skipped = skipped;
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{DuplexStream, ReadBuf};

    use super::super::mission_hook;
    use super::super::subscription::ConnStats;
    use super::*;
    use crate::cot_store::CotStoreHandle;

    /// A connection whose client says nothing for long enough to be noticed.
    const IDLE: Duration = Duration::from_secs(90);

    /// The dependencies a read loop needs: a router over an empty hub, and a
    /// database nothing is recorded to.
    async fn deps() -> ConnDeps {
        let metrics = Arc::new(StreamMetrics::default());
        let router = Router::new(
            Arc::new(super::super::hub::Hub::new()),
            crate::db::Database::open_in_memory().await.unwrap(),
            CotStoreHandle::disabled(),
            mission_hook::no_missions(),
            Arc::clone(&metrics),
            "rustak-test",
        );

        ConnDeps {
            router: Arc::new(router),
            metrics,
            server_version: "rustak-test".to_string(),
            public_url: None,
            limits: ConnLimits {
                max_frame: 1 << 20,
                queue_len: 8,
                close_after_drops: 512,
                idle_timeout: IDLE,
                write_timeout: Duration::from_secs(5),
                negotiate: NegotiationMode::Accept,
            },
        }
    }

    /// A handle, the liveness it shares, and the queue the caller must hold.
    ///
    /// The receiver comes back rather than being dropped here: a dropped one
    /// makes every `send` report the connection closed, which is a different
    /// story from the one any of these tests is telling.
    fn handle(closing: &Shutdown) -> (ConnHandle, Arc<Liveness>, mpsc::Receiver<Outbound>) {
        let liveness = Arc::new(Liveness::new());
        let (tx, rx) = mpsc::channel(8);

        let handle = ConnHandle::new(
            ConnId(1),
            tx,
            Arc::new(ConnStats::default()),
            512,
            closing.clone(),
        )
        .with_liveness(Arc::clone(&liveness));

        (handle, liveness, rx)
    }

    /// Runs the read loop over `io` until it ends, and answers why it did.
    async fn read_until_it_ends(
        deps: ConnDeps,
        io: impl AsyncRead + Unpin + Send + 'static,
        handle: ConnHandle,
        closing: Shutdown,
    ) -> tokio::task::JoinHandle<LeaveReason> {
        let max_frame = deps.limits.max_frame;

        tokio::spawn(async move {
            let mut framed = FramedRead::new(io, TakCodec::with_limit(Mode::Xml, max_frame));
            let mut negotiation =
                Negotiation::with_mode(NegotiationMode::Accept, "rustak-test".to_string());

            read_loop(
                &deps,
                &mut framed,
                &handle,
                &mut negotiation,
                ConnId(1),
                &closing,
            )
            .await
        })
    }

    /// A socket that fails rather than ending.
    struct Broken;

    impl AsyncRead for Broken {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::from(io::ErrorKind::ConnectionReset)))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_is_being_written_to_is_never_reclaimed_for_saying_nothing() {
        // M9-15, the whole point. FIRMS receives the channel's traffic and has
        // nothing of its own to publish, so it sends nothing for hours — and a
        // read-idle timer reclaimed it every ninety seconds while the server
        // was happily writing to it.
        let deps = deps().await;
        let closing = Shutdown::new();
        let (handle, liveness, _queue) = handle(&closing);
        // Held so the socket does not reach end-of-file.
        let (_peer, socket): (DuplexStream, DuplexStream) = tokio::io::duplex(1024);

        let reading = read_until_it_ends(deps, socket, handle, closing.clone()).await;

        // Ten times the idle timeout, one write a minute, not one byte
        // received.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_secs(60)).await;
            liveness.wrote();
            tokio::task::yield_now().await;

            assert!(
                !reading.is_finished(),
                "a connection being written to is not idle, whatever the client is doing",
            );
        }

        closing.cancel();

        assert_eq!(
            reading.await.expect("the read loop does not panic"),
            LeaveReason::Shutdown,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_with_nothing_in_either_direction_is_reclaimed_as_idle() {
        // The case the timeout is actually for: a device that fell off the
        // network without closing its socket. Nothing is written to it and
        // nothing arrives from it, so the client would have pinged by now.
        let deps = deps().await;
        let closing = Shutdown::new();
        let (handle, _liveness, _queue) = handle(&closing);
        let (_peer, socket) = tokio::io::duplex(1024);

        let reading = read_until_it_ends(deps, socket, handle, closing).await;

        assert_eq!(
            reading.await.expect("the read loop does not panic"),
            LeaveReason::Idle,
            "the paused clock advances to the deadline because nothing else can run",
        );
    }

    #[tokio::test]
    async fn a_client_that_closes_its_end_is_not_a_failure() {
        let deps = deps().await;
        let closing = Shutdown::new();
        let (handle, _liveness, _queue) = handle(&closing);
        let (peer, socket) = tokio::io::duplex(1024);
        drop(peer);

        let reading = read_until_it_ends(deps, socket, handle, closing).await;

        assert_eq!(reading.await.unwrap(), LeaveReason::ClientClosed);
    }

    #[tokio::test]
    async fn a_socket_that_fails_is_told_apart_from_a_client_that_left() {
        let deps = deps().await;
        let closing = Shutdown::new();
        let (handle, _liveness, _queue) = handle(&closing);

        let reading = read_until_it_ends(deps, Broken, handle, closing).await;

        assert_eq!(reading.await.unwrap(), LeaveReason::ReadError);
    }

    #[tokio::test]
    async fn a_close_from_outside_carries_the_cause_it_was_given() {
        // A revoked certificate, an account switched off, an administrator, a
        // consumer too far behind: each one cancels the same token, and the
        // disconnect line has to say which.
        for cause in [
            LeaveReason::Revoked,
            LeaveReason::AccountDisabled,
            LeaveReason::Administrator,
            LeaveReason::SlowConsumer,
        ] {
            let deps = deps().await;
            let closing = Shutdown::new();
            let (handle, _liveness, _queue) = handle(&closing);
            let (_peer, socket) = tokio::io::duplex(1024);

            let reading = read_until_it_ends(deps, socket, handle.clone(), closing).await;
            handle.close(cause);

            assert_eq!(reading.await.unwrap(), cause);
        }
    }

    #[tokio::test]
    async fn the_server_stopping_is_its_own_cause() {
        let deps = deps().await;
        let shutdown = Shutdown::new();
        let closing = shutdown.child();
        let (handle, _liveness, _queue) = handle(&closing);
        let (_peer, socket) = tokio::io::duplex(1024);

        let reading = read_until_it_ends(deps, socket, handle, closing).await;
        shutdown.cancel();

        assert_eq!(
            reading.await.unwrap(),
            LeaveReason::Shutdown,
            "a cancellation nobody explained is the listener draining",
        );
    }
}
