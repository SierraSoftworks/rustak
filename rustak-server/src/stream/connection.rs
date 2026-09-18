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
//!    quiet for longer than the idle timeout.
//! 5. Unregister, *then* tell the peers it has gone.
//!
//! Steps 2 and 3 both go through the per-connection writer queue, which is what
//! makes "replay before offer" true without a wait: one queue, one writer, one
//! order.
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

use crate::prelude::*;

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
    /// Whether it is offered protobuf.
    pub negotiate: bool,
}

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
    let handle = ConnHandle::new(
        id,
        tx,
        Arc::clone(&stats),
        deps.limits.close_after_drops,
        closing.clone(),
    );

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
    let writing = tokio::spawn(writer::run(
        rx,
        sink,
        writer::WriterContext {
            metrics: Arc::clone(&deps.metrics),
            public_url: deps.public_url.clone(),
        },
    ));

    replay::replay_latest_sa(&hub, id);

    let mut negotiation = Negotiation::new(deps.limits.negotiate, deps.server_version.clone());
    if let Some(offer) = negotiation.offer(uuid::Uuid::new_v4().to_string(), CotTime::now()) {
        handle.send(Outbound::Event(Arc::new(EncodedEvent::new(offer))));
    }

    info!("A client is on the stream.");

    let mut framed = FramedRead::new(read, TakCodec::with_limit(Mode::Xml, deps.limits.max_frame));
    read_loop(&deps, &mut framed, &handle, &mut negotiation, id, &closing).await;

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
    handle.close();
    drop(handle);

    let _ = tokio::time::timeout(Duration::from_secs(5), writing).await;

    info!(
        rx = stats.rx_msgs.load(std::sync::atomic::Ordering::Relaxed),
        tx = stats.tx_msgs.load(std::sync::atomic::Ordering::Relaxed),
        dropped = stats.dropped.load(std::sync::atomic::Ordering::Relaxed),
        "A client left the stream."
    );
}

/// Reads until the connection ends, routing everything it understands.
async fn read_loop<R: AsyncRead + Unpin>(
    deps: &ConnDeps,
    framed: &mut FramedRead<R, TakCodec>,
    handle: &ConnHandle,
    negotiation: &mut Negotiation,
    id: ConnId,
    closing: &Shutdown,
) {
    let mut counters = Counters::default();

    loop {
        let next = tokio::select! {
            biased;

            () = closing.cancelled() => break,
            next = tokio::time::timeout(deps.limits.idle_timeout, framed.next()) => next,
        };

        let frame = match next {
            Err(_) => {
                debug!("A stream connection went quiet and was reclaimed.");
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(err))) => {
                debug!(error = %err, "A stream connection failed while reading.");
                break;
            }
            Ok(Some(Ok(frame))) => frame,
        };

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
