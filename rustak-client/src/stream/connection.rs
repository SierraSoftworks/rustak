//! One TAK stream connection: framing, negotiation, keepalive and events.
//!
//! [`TakStream`] is a [`Stream`] of inbound [`Event`]s and a [`Sink`] for
//! outbound ones. Everything the protocol requires a client to do by itself
//! happens inside `poll_next` — the negotiation exchange, the keepalive ping,
//! the death clock — so a caller's loop is the one it would write anyway:
//!
//! ```no_run
//! use futures::{SinkExt, StreamExt};
//! use rustak_client::stream::{StreamConfig, TakStream, connect};
//!
//! # async fn example(config: StreamConfig) -> Result<(), rustak_client::stream::StreamError> {
//! let mut stream: TakStream = connect(&config).await?;
//!
//! while let Some(event) = stream.next().await {
//!     let event = event?;
//!     println!("{} from {}", event.r#type, event.uid);
//! }
//! # Ok(()) }
//! ```
//!
//! # What it hides, and what it does not
//!
//! Hidden: the `t-x-takp-*` exchange, the switch to protobuf, our own pings,
//! and the server's pongs. A caller that wants to see those — a conformance
//! test, mostly — sets [`StreamConfig::pass_control`], which delivers them
//! *as well as* acting on them.
//!
//! Not hidden: a message we could not parse. It is counted
//! ([`dropped`](TakStream::dropped)) and skipped, never surfaced as an error,
//! because one bad message must not cost the connection (`M1-03`).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures::{Sink, Stream};
use rustak_cot::codec::{EncodedEvent, Frame, Mode, TakCodec};
use rustak_cot::error::CodecError;
use rustak_cot::types::cot_type;
use rustak_cot::{CotTime, Event, msgs, negotiate, proto, xml};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{Instant, Sleep, sleep_until};
use tokio_util::codec::Framed;

use super::keepalive::{KeepaliveState, Tick};
use super::negotiation::ClientNeg;
use super::{Negotiation, StreamConfig, StreamError};

/// Anything a TAK stream can run over: a TLS session, a TCP socket, or an
/// in-memory duplex in a test.
pub trait AsyncIo: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncIo for T {}

/// The framed transport, with the codec whose mode negotiation flips.
type Wire = Framed<Box<dyn AsyncIo>, TakCodec>;

/// A connected TAK stream.
pub struct TakStream {
    wire: Wire,
    neg: ClientNeg,
    neg_timer: Option<Pin<Box<Sleep>>>,
    keepalive: KeepaliveState,
    /// Events the caller handed us while negotiation was outstanding.
    pending: VecDeque<Event>,
    /// Events waiting for the socket to accept them.
    outbox: VecDeque<EncodedEvent>,
    /// Events that arrived while [`settle`](TakStream::settle) was running.
    inbox: VecDeque<Event>,
    uid: String,
    pass_control: bool,
    dropped_parse: u64,
    done: bool,
}

impl TakStream {
    /// Wraps an already-connected transport.
    ///
    /// [`connect`](super::connect) is what opens one; this is the seam a test
    /// harness uses with [`tokio::io::duplex`], and the one an interop suite
    /// uses with a transport of its own.
    pub fn new(io: Box<dyn AsyncIo>, config: &StreamConfig) -> Self {
        Self {
            wire: Framed::new(io, TakCodec::new(Mode::Xml)),
            neg: ClientNeg::new(config.negotiate),
            neg_timer: None,
            keepalive: KeepaliveState::new(config.keepalive),
            pending: VecDeque::new(),
            outbox: VecDeque::new(),
            inbox: VecDeque::new(),
            uid: config.uid.clone(),
            pass_control: config.pass_control,
            dropped_parse: 0,
            done: false,
        }
    }

    /// Wraps a transport with the default behaviour: negotiate, ATAK's
    /// keepalive, control messages hidden.
    pub fn over(io: impl AsyncIo + 'static, uid: impl Into<String>) -> Self {
        let config = StreamConfig::new(super::Endpoint::tls("in-memory", 0), uid);

        Self::new(Box::new(io), &config)
    }

    /// The uid this client pings with, and the one its own events carry.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// Which encoding the connection is speaking right now.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.wire.codec().mode()
    }

    /// Where protocol negotiation has got to.
    #[must_use]
    pub fn negotiation(&self) -> Negotiation {
        self.neg.state()
    }

    /// The server version the `t-x-takp-v` offer announced, if there was one.
    #[must_use]
    pub fn server_version(&self) -> Option<&str> {
        self.neg.server_version()
    }

    /// Whether control messages are delivered to the caller as well as acted on.
    pub fn set_pass_control(&mut self, pass: bool) {
        self.pass_control = pass;
    }

    /// How many inbound messages were thrown away: unparseable ones, and ones
    /// the codec could not frame.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped_parse
            .saturating_add(self.wire.codec().dropped())
    }

    /// How many bytes the codec skipped resynchronising a protobuf stream.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.wire.codec().skipped()
    }

    /// How many outbound events are waiting: held back by an outstanding
    /// negotiation, or waiting for the socket to take them.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.pending.len() + self.outbox.len()
    }

    /// Runs the connection's own protocol work until it has nothing left to
    /// write, holding any events that arrive meanwhile for the next read.
    ///
    /// This exists because a [`Stream`] only acts when it is polled, and an
    /// event handed to [`Sink::start_send`] while protocol negotiation is
    /// outstanding is *held* until the answer arrives (see
    /// [`queued`](Self::queued)). A caller whose loop is `next().await` never
    /// notices; one that sends and then waits for the effect on somebody else
    /// would wait forever. `budget` caps how long to wait for the connection to
    /// go quiet — running out is not an error, because a busy connection is not
    /// a broken one.
    ///
    /// # Errors
    ///
    /// Whatever the connection would have returned from its next read.
    pub async fn settle(&mut self, budget: std::time::Duration) -> Result<(), StreamError> {
        while self.queued() > 0 {
            // Deliberately not `self.next()`: that would hand back an event
            // this loop had already stashed, and stash it again, forever.
            let outcome = {
                let read = std::future::poll_fn(|cx| self.poll_event(cx));

                tokio::time::timeout(budget, read).await
            };

            match outcome {
                Ok(Some(Ok(event))) => self.inbox.push_back(event),
                Ok(Some(Err(error))) => return Err(error),
                Ok(None) | Err(_) => return Ok(()),
            }
        }

        Ok(())
    }

    /// The read path, without the [`settle`](Self::settle) inbox.
    ///
    /// Everything the connection does for itself happens here: the writes it
    /// owes, the negotiation deadline, the keepalive, and one framed message.
    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Event, StreamError>>> {
        if self.done {
            return Poll::Ready(None);
        }

        loop {
            // Anything we owe the socket goes out first: a negotiation request
            // the server is waiting on, or a ping whose whole purpose is to be
            // written before we block on a read.
            if let Poll::Ready(Err(error)) = self.poll_outbox(cx) {
                self.done = true;
                return Poll::Ready(Some(Err(error.into())));
            }

            // 60 s with no `t-x-takp-r`: carry on in XML, which is what the
            // reference client does and what a server that does not implement
            // the exchange relies on.
            if let Some(timer) = self.neg_timer.as_mut()
                && timer.as_mut().poll(cx).is_ready()
            {
                tracing::debug!(
                    "The server never answered our protocol negotiation; staying on XML."
                );
                let state = self.neg.expire();
                self.adopt(state);
                continue;
            }

            match self.keepalive.poll(cx, self.neg.state().is_quiet()) {
                Tick::Dead => {
                    tracing::warn!(
                        silence = ?self.keepalive.silence(),
                        uid = %self.uid,
                        "The TAK server stopped answering; treating the connection as dead.",
                    );
                    self.done = true;
                    return Poll::Ready(Some(Err(StreamError::RxTimeout)));
                }
                Tick::Ping => {
                    let ping = msgs::ping(&self.uid, CotTime::now());
                    self.emit(ping);
                    continue;
                }
                Tick::Quiet => {}
            }

            let frame = match ready!(Pin::new(&mut self.wire).poll_next(cx)) {
                Some(Ok(frame)) => frame,
                Some(Err(error)) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(error.into())));
                }
                None => {
                    self.done = true;
                    return Poll::Ready(None);
                }
            };

            // The peer is alive whatever it sent, so the clock restarts before
            // we decide whether we can read it.
            self.keepalive.record_rx();

            let event = match parse(&frame) {
                Ok(event) => event,
                Err(error) => {
                    self.dropped_parse = self.dropped_parse.saturating_add(1);
                    tracing::debug!(%error, "Dropped a message we could not read.");
                    continue;
                }
            };

            let consumed = self.control(&event);
            if consumed && !self.pass_control {
                continue;
            }

            // Anything the message obliged us to write — a negotiation
            // request, most importantly — goes out before we hand the caller
            // an event and stop being polled.
            if !self.outbox.is_empty()
                && let Poll::Ready(Err(error)) = self.poll_outbox(cx)
            {
                self.done = true;
                return Poll::Ready(Some(Err(error.into())));
            }

            return Poll::Ready(Some(Ok(event)));
        }
    }

    /// Queues an event we generate ourselves — a ping, a negotiation request.
    fn emit(&mut self, event: Event) {
        self.outbox.push_back(EncodedEvent::new(event));
    }

    /// Releases the events held back during negotiation, now that the encoding
    /// they will be written in is settled.
    fn release(&mut self) {
        while let Some(event) = self.pending.pop_front() {
            self.outbox.push_back(EncodedEvent::new(event));
        }
    }

    /// Writes as much of the outbox as the transport will take.
    ///
    /// Every frame that goes out restarts the outbound keepalive clock,
    /// whatever it was — a position report, a chat message or a ping. A client
    /// that publishes regularly therefore never sends a keepalive of its own;
    /// one that publishes nothing sends one every
    /// [`Keepalive::outbound_idle`](super::Keepalive::outbound_idle), which is
    /// what keeps a receive-only sidecar off a server's idle list (M9-15).
    fn poll_outbox(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), CodecError>> {
        while !self.outbox.is_empty() {
            ready!(Sink::<&EncodedEvent>::poll_ready(
                Pin::new(&mut self.wire),
                cx
            ))?;

            let Some(encoded) = self.outbox.pop_front() else {
                break;
            };

            Sink::<&EncodedEvent>::start_send(Pin::new(&mut self.wire), &encoded)?;
            self.keepalive.record_tx();
        }

        Sink::<&EncodedEvent>::poll_flush(Pin::new(&mut self.wire), cx)
    }

    /// Applies a settled negotiation: the codec's mode, and the held events.
    fn adopt(&mut self, state: Negotiation) {
        if state == Negotiation::Proto {
            self.wire.codec_mut().set_mode(Mode::Proto);
        }

        self.neg_timer = None;
        self.release();
    }

    /// Handles an inbound control message, returning whether it was consumed.
    fn control(&mut self, event: &Event) -> bool {
        match event.r#type.as_str() {
            cot_type::TAKP_V => {
                if let Some(request) = self.neg.on_announce(event, CotTime::now()) {
                    self.emit(request);
                    self.neg_timer =
                        Some(Box::pin(sleep_until(Instant::now() + negotiate::VALIDITY)));
                }
                true
            }
            cot_type::TAKP_R => {
                if let Some(state) = self.neg.on_response(event) {
                    tracing::debug!(?state, "The server answered our protocol negotiation.");
                    self.adopt(state);
                }
                true
            }
            cot_type::TAKP_Q => true,
            _ => msgs::is_pong(event),
        }
    }
}

/// Turns a framed message into an event, whichever encoding it arrived in.
fn parse(frame: &Frame) -> Result<Event, CodecError> {
    match frame {
        Frame::Xml(bytes) => Ok(xml::parse(bytes)?),
        Frame::Proto(payload) => Ok(proto::message_to_event(proto::decode(payload)?)?),
    }
}

impl Stream for TakStream {
    type Item = Result<Event, StreamError>;

    /// Delivers whatever [`settle`](Self::settle) stashed first, then reads.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        match this.inbox.pop_front() {
            Some(event) => Poll::Ready(Some(Ok(event))),
            None => this.poll_event(cx),
        }
    }
}

impl Sink<Event> for TakStream {
    type Error = StreamError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        let this = self.get_mut();

        Poll::Ready(ready!(this.poll_outbox(cx)).map_err(Into::into))
    }

    /// Queues an event, or holds it back if negotiation is outstanding.
    ///
    /// Between our `t-x-takp-q` and the server's answer, the encoding the
    /// server will read the next byte in is undecided — so the event waits
    /// rather than going out in one the server may already have left.
    fn start_send(self: Pin<&mut Self>, event: Event) -> Result<(), StreamError> {
        let this = self.get_mut();

        match this.neg.state().is_quiet() {
            true => this.pending.push_back(event),
            false => this.outbox.push_back(EncodedEvent::new(event)),
        }

        Ok(())
    }

    /// Flushes what can be written.
    ///
    /// Events held back by an outstanding negotiation are *not* flushed —
    /// they cannot be, without choosing an encoding — so this reports ready
    /// with [`queued`](TakStream::queued) still non-zero. They go out when
    /// the server answers, or when the offer's 60 seconds run out.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        let this = self.get_mut();

        Poll::Ready(ready!(this.poll_outbox(cx)).map_err(Into::into))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        let this = self.get_mut();

        ready!(this.poll_outbox(cx)).map_err(StreamError::from)?;
        this.done = true;

        Poll::Ready(
            ready!(Sink::<&EncodedEvent>::poll_close(
                Pin::new(&mut this.wire),
                cx
            ))
            .map_err(Into::into),
        )
    }
}

impl std::fmt::Debug for TakStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TakStream")
            .field("uid", &self.uid)
            .field("mode", &self.mode())
            .field("negotiation", &self.negotiation())
            .field("queued", &self.queued())
            .field("dropped", &self.dropped())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{FutureExt, SinkExt, StreamExt};
    use tokio::io::DuplexStream;
    use tokio_util::codec::Framed;

    use super::super::{Endpoint, Keepalive};
    use super::*;

    /// The server's side of the socket: whatever the client writes, in frames.
    type Peer = Framed<DuplexStream, TakCodec>;

    /// A client that negotiates nothing, so the only thing it ever writes is
    /// what the keepalive decides to write.
    fn connected(keepalive: Keepalive) -> (TakStream, Peer) {
        let (mine, theirs) = tokio::io::duplex(1 << 16);
        let config = StreamConfig::new(Endpoint::tls("in-memory", 0), "SERVICE-firms")
            .with_negotiation(false)
            .with_keepalive(keepalive);

        (
            TakStream::new(Box::new(mine), &config),
            Framed::new(theirs, TakCodec::new(Mode::Xml)),
        )
    }

    /// One relayed position report, as the server would send it.
    fn broadcast(uid: &str) -> Event {
        Event::builder("a-f-G-U-C", uid).point(51.5, -0.12).build()
    }

    /// Everything the client has written and not yet been read, without
    /// waiting for anything that has not been written.
    fn drain(peer: &mut Peer) -> Vec<Event> {
        let mut seen = Vec::new();

        while let Some(Some(Ok(frame))) = peer.next().now_or_never() {
            if let Ok(event) = parse(&frame) {
                seen.push(event);
            }
        }

        seen
    }

    /// Lets the connection task run whatever the clock has just made ready.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_only_receives_keeps_its_own_connection_alive() {
        // M9-15, on the wire. FIRMS hears the channel's traffic every few
        // seconds and has nothing to publish, so ATAK's inbound rule never
        // fires; without the outbound one it would write nothing at all and
        // any server with a read-idle timer would reclaim it.
        let (stream, mut peer) = connected(Keepalive::DEFAULT);
        let reading = tokio::spawn(async move {
            let mut stream = stream;

            while let Some(Ok(_)) = stream.next().await {}
        });

        let mut pings = 0;
        for second in 1..=120 {
            tokio::time::advance(Duration::from_secs(1)).await;

            if second % 10 == 0 {
                peer.send(&EncodedEvent::new(broadcast("UID-AIS")))
                    .await
                    .expect("the server writes to a client that is reading");
            }

            settle().await;
            pings += drain(&mut peer)
                .iter()
                .filter(|event| event.r#type == cot_type::PING)
                .count();
        }

        assert_eq!(
            pings, 4,
            "one ping per 30s of having written nothing, over two minutes",
        );
        assert!(!reading.is_finished(), "and the connection is still up");

        reading.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_publishes_never_needs_a_keepalive_of_its_own() {
        // The other half: a sidecar that writes every ten seconds resets the
        // outbound clock every time, so it costs the connection nothing.
        let (mut stream, mut peer) = connected(Keepalive::DEFAULT);

        let mut pings = 0;
        for second in 1..=120 {
            tokio::time::advance(Duration::from_secs(1)).await;

            if second % 10 == 0 {
                peer.send(&EncodedEvent::new(broadcast("UID-AIS")))
                    .await
                    .unwrap();
                stream
                    .send(broadcast("UID-FIRMS"))
                    .await
                    .expect("the sidecar publishes");
            }

            // Reads what the server sent, which is what makes the connection
            // act on its own clocks.
            let _ = std::future::poll_fn(|cx| stream.poll_event(cx)).now_or_never();
            pings += drain(&mut peer)
                .iter()
                .filter(|event| event.r#type == cot_type::PING)
                .count();
        }

        assert_eq!(pings, 0, "a client that publishes is never outbound-silent");
    }

    #[tokio::test(start_paused = true)]
    async fn a_keepalive_that_is_off_writes_nothing_at_all() {
        // What the server's own integration tests connect with: the test
        // decides what the client sends, and the client adds nothing.
        let (stream, mut peer) = connected(Keepalive::OFF);
        let reading = tokio::spawn(async move {
            let mut stream = stream;

            while let Some(Ok(_)) = stream.next().await {}
        });

        for _ in 0..12 {
            tokio::time::advance(Duration::from_secs(10)).await;
            peer.send(&EncodedEvent::new(broadcast("UID-AIS")))
                .await
                .unwrap();
            settle().await;

            assert!(drain(&mut peer).is_empty(), "nothing is ever written");
        }

        assert!(!reading.is_finished(), "and nothing is ever given up on");

        reading.abort();
    }
}
