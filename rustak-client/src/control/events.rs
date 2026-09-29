//! Consuming `GET /api/v1/events` as a [`Stream`] of [`ServerEvent`]s.
//!
//! ```no_run
//! # async fn example(control: &rustak_client::control::ControlClient) -> Result<(), human_errors::Error> {
//! use futures::StreamExt;
//! use rustak_client::control::ServerEventPayload;
//!
//! let mut events = control.events(None).await?;
//!
//! while let Some(event) = events.next().await {
//!     if let ServerEventPayload::ClientConnected(client) = &event.payload {
//!         println!("{} joined", client.username);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What this stream does not do
//!
//! Reconnect. The stream ends when the connection does, and the caller decides
//! what to do about it — which for a plugin is the harness, whose
//! [`Sidecar`](crate::sidecar::Sidecar) loop reopens the feed from the last id it
//! saw. Putting the retry here would mean a stream that can never be exhausted,
//! and a plugin that wanted to stop waiting would have nothing to wait on.
//!
//! # It is held open for hours, not for thirty seconds
//!
//! The feed is a body read until something ends it, so it is opened through
//! [`http::feed_client`](crate::http::feed_client) rather than the client the
//! ordinary control-API calls use: no *total* timeout, a connect timeout, and a
//! read timeout that the server's own keep-alive comments reset. The first live
//! deployment ran it through the ordinary client and every feed was cut at
//! `reqwest`'s thirty-second deadline — a reopening every 31 seconds, which
//! reads as a fault and is not one.
//!
//! What ends a feed for real is silence: no byte, not even a keep-alive
//! comment, for [`FEED_IDLE_TIMEOUT`](crate::http::FEED_IDLE_TIMEOUT). That
//! arrives here as an error on the body, which ends the stream exactly as a
//! clean close does, and the caller reopens.
//!
//! # Forward compatibility is a skipped frame
//!
//! An event whose `type` this build has never heard of will not deserialise, and
//! is logged and skipped rather than ending the stream. That is what lets a
//! plugin built against today's `rustak-api` keep running against a server that
//! has learned to announce something new.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use rustak_core::prelude::*;

use super::{ControlClient, succeeded};

/// Re-exported so a plugin never has to name `rustak-api` in its own manifest:
/// what a sidecar reads off the feed is part of this crate's surface.
pub use rustak_api::event::{
    ChannelEvent, ClientEvent, MissionEvent, PackageEvent, ServerEvent, ServerEventPayload,
    ServiceEvent,
};

/// The largest a single SSE frame may be before we give up on it.
///
/// A frame is one event, and the events on this feed are a few hundred bytes. A
/// megabyte means the other end is not what we think it is — a proxy error page,
/// a captive portal — and buffering it forever is how a sidecar runs out of
/// memory quietly.
const MAX_FRAME: usize = 1024 * 1024;

impl ControlClient {
    /// Opens the server-event feed.
    ///
    /// `after` is the id of the last event this consumer saw, which the server
    /// resumes from — pass it on a reconnection and nothing published while the
    /// feed was down is missed, as long as it was down for fewer than the
    /// server's ring of events.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the credential is refused
    /// (`401`), the account is neither an administrator nor a service (`403`),
    /// or the server cannot be reached.
    pub async fn events(&self, after: Option<u64>) -> Result<EventStream, Error> {
        // The feed's own client: no total timeout, an idle timeout instead.
        // See `http::feed_client`.
        let mut request = self.request_on(self.feed_http(), reqwest::Method::GET, "/events");

        if let Some(after) = after {
            request = request.header("Last-Event-ID", after.to_string());
        }

        let what = "open the server-event feed";
        let response = succeeded(self.raw(request, what).await?, what).await?;

        Ok(EventStream::new(response))
    }
}

/// The server-event feed, as a stream.
///
/// Ends when the connection does. Nothing here is an error: a feed that closed
/// is a feed to reopen, and the failure that closed it has already been logged.
pub struct EventStream {
    body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    /// Bytes received and not yet split into a frame.
    buffer: String,
    /// The id of the last event handed out, for a caller that reconnects.
    last_id: Option<u64>,
    /// Whether the body has ended.
    done: bool,
}

impl std::fmt::Debug for EventStream {
    /// Written out because the body is a boxed stream, which has no `Debug` —
    /// and a plugin that logs its own state at start-up should not have to
    /// special-case this one field.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventStream")
            .field("last_id", &self.last_id)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl EventStream {
    /// Wraps a `text/event-stream` response.
    fn new(response: reqwest::Response) -> Self {
        Self {
            body: Box::pin(response.bytes_stream()),
            buffer: String::new(),
            last_id: None,
            done: false,
        }
    }

    /// The id of the last event this stream produced.
    ///
    /// Hand it back to [`ControlClient::events`] on a reconnection.
    pub fn last_id(&self) -> Option<u64> {
        self.last_id
    }

    /// Takes the next complete frame out of the buffer, if there is one.
    ///
    /// SSE separates frames with a blank line, and a server may use either line
    /// ending; both are accepted because a proxy in the middle is entitled to
    /// rewrite them.
    fn take_frame(&mut self) -> Option<String> {
        let end = [self.buffer.find("\n\n"), self.buffer.find("\r\n\r\n")]
            .into_iter()
            .flatten()
            .min()?;
        let frame = self.buffer[..end].to_string();
        let skip = if self.buffer[end..].starts_with("\r\n\r\n") {
            4
        } else {
            2
        };
        self.buffer.drain(..end + skip);

        Some(frame)
    }
}

impl Stream for EventStream {
    type Item = ServerEvent;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            while let Some(frame) = this.take_frame() {
                if let Some(event) = parse(&frame) {
                    this.last_id = Some(event.id);

                    return Poll::Ready(Some(event));
                }
            }

            if this.done {
                return Poll::Ready(None);
            }

            match this.body.poll_next_unpin(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.done = true;

                    // Round again: a final frame with no trailing blank line is
                    // still a frame, and `take_frame` will not have seen it.
                    if !this.buffer.trim().is_empty() {
                        let remainder = std::mem::take(&mut this.buffer);

                        if let Some(event) = parse(&remainder) {
                            this.last_id = Some(event.id);

                            return Poll::Ready(Some(event));
                        }
                    }

                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Err(err))) => {
                    debug!(error = %err, "The server-event feed ended.");
                    this.done = true;

                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.push_str(&String::from_utf8_lossy(&chunk));

                    if this.buffer.len() > MAX_FRAME {
                        warn!(
                            bytes = this.buffer.len(),
                            "A server-event frame grew past what we will buffer; ending the feed."
                        );
                        this.done = true;

                        return Poll::Ready(None);
                    }
                }
            }
        }
    }
}

/// One SSE frame's `data:` payload, as an event.
///
/// Answers [`None`] for anything that is not one — a comment (the keepalive), a
/// `retry:` preamble, or an event whose `type` this build does not know.
fn parse(frame: &str) -> Option<ServerEvent> {
    let mut data = String::new();

    for line in frame.lines() {
        // A line starting with a colon is a comment, which is what the
        // keepalive is.
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };

        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(value.trim_start());
    }

    if data.trim().is_empty() {
        return None;
    }

    match serde_json::from_str(&data) {
        Ok(event) => Some(event),
        Err(err) => {
            debug!(error = %err, "Skipped a server event this build does not understand.");

            None
        }
    }
}

#[cfg(test)]
mod tests {
    use rustak_core::identity::ServiceName;
    use rustak_core::service::ServiceIdentity;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const FEED: &str = concat!(
        "retry: 5000\n\n",
        ": keep-alive\n\n",
        "id: 1\nevent: client.connected\n",
        "data: {\"id\":1,\"at\":\"2026-09-18T12:00:00.000Z\",\"type\":\"client.connected\",\"username\":\"ada\",\"uid\":\"ANDROID-1\"}\n\n",
        "id: 2\nevent: weather.rained\n",
        "data: {\"id\":2,\"at\":\"2026-09-18T12:00:01.000Z\",\"type\":\"weather.rained\"}\n\n",
        "id: 3\nevent: channel.changed\n",
        "data: {\"id\":3,\"at\":\"2026-09-18T12:00:02.000Z\",\"type\":\"channel.changed\",\"username\":\"ada\"}\n\n",
    );

    async fn feed(server: &MockServer, body: &str) -> EventStream {
        let identity = ServiceIdentity::new(ServiceName::parse("weather").unwrap())
            .with_credential(Secret::new("rsk_secret"));
        Mock::given(method("GET"))
            .and(path("/api/v1/events"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(server)
            .await;

        ControlClient::with_http(&server.uri(), reqwest::Client::new(), &identity)
            .unwrap()
            .events(None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_feed_yields_events_and_skips_everything_that_is_not_one() {
        // The three things a consumer must ignore rather than choke on: the
        // `retry:` preamble, a keepalive comment, and an event a later server
        // learned to send.
        let server = MockServer::start().await;
        let mut stream = feed(&server, FEED).await;

        let first = stream.next().await.expect("the first event");
        let second = stream.next().await.expect("the second event");

        assert_eq!(first.id, 1);
        assert_eq!(first.name(), "client.connected");
        assert_eq!(second.id, 3, "the unknown event was skipped, not fatal");
        assert_eq!(stream.last_id(), Some(3));
        assert!(stream.next().await.is_none(), "the body ended");
    }

    #[tokio::test]
    async fn a_frame_with_no_trailing_blank_line_is_still_delivered() {
        // What a server that was stopped mid-write leaves behind.
        let server = MockServer::start().await;
        let mut stream = feed(
            &server,
            "id: 7\nevent: channel.changed\ndata: {\"id\":7,\"at\":\"2026-09-18T12:00:00.000Z\",\"type\":\"channel.changed\",\"username\":\"ada\"}",
        )
        .await;

        assert_eq!(stream.next().await.map(|event| event.id), Some(7));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn resuming_sends_the_id_the_consumer_last_saw() {
        let server = MockServer::start().await;
        let identity = ServiceIdentity::new(ServiceName::parse("weather").unwrap());
        Mock::given(method("GET"))
            .and(path("/api/v1/events"))
            .and(header("last-event-id", "41"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;

        let opened = ControlClient::with_http(&server.uri(), reqwest::Client::new(), &identity)
            .unwrap()
            .events(Some(41))
            .await;

        assert!(opened.is_ok());
    }

    /// The feed's timeouts, on a clock the test holds.
    ///
    /// What is under test here is time — a feed that goes silent is ended, one
    /// kept alive by comments is not, nothing cuts a feed for being old, and a
    /// server that never answers is given up on — so these used to run on real
    /// timers against a real socket, and a scheduling stall as long as the
    /// shortest of them failed them (M9-13, then `36f22293`, which widened the
    /// margins without removing the cause).
    ///
    /// Now the clock is tokio's paused one, and **it never moves on its own**.
    /// A paused clock normally jumps to the next timer whenever the runtime has
    /// nothing to do — which, with real loopback I/O in flight, can fire a
    /// timeout before bytes already on their way are read. So nothing here is
    /// simply `.await`ed: [`frozen`] polls a future and yields between polls,
    /// and a runtime with a task to yield to never idles, so it never advances
    /// the clock. The only thing that moves it is `tokio::time::advance`,
    /// called by the test. `reqwest`'s own connect, read and total timeouts are
    /// `tokio::time` sleeps (checked in its source, 0.13.5), so they run on
    /// this clock too.
    ///
    /// Real time is still spent waiting for loopback I/O, and a slow host
    /// spends more of it — but no timer can fire while it does, so how long it
    /// takes cannot change what the test sees. [`HUNG`] is the only real-time
    /// bound, and it is there to fail a hung test, not to measure one.
    ///
    /// Every client here is built by the production constructors
    /// ([`ControlClient::new`], [`http::client`]), so the numbers asserted are
    /// the shipped ones — [`FEED_IDLE_TIMEOUT`], [`CONNECT_TIMEOUT`] and
    /// [`DEFAULT_TIMEOUT`] — at no cost in wall time.
    mod timeouts {
        use std::future::Future;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::Poll;
        use std::time::Duration;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::time::{Instant, advance};

        use super::*;
        use crate::http::{self, CONNECT_TIMEOUT, DEFAULT_TIMEOUT, FEED_IDLE_TIMEOUT, Trust};

        /// How much *real* time a frozen wait gets before the test is called
        /// hung. Nothing is compared against it.
        const HUNG: Duration = Duration::from_secs(60);

        /// How often the server writes its keep-alive comment into an idle
        /// feed (`KEEPALIVE`, in the server's `web::api::events`).
        const KEEPALIVE: Duration = Duration::from_secs(20);

        /// The comment itself.
        const COMMENT: &str = ": keep-alive\n\n";

        /// The distance either side of a deadline.
        const TICK: Duration = Duration::from_millis(1);

        /// Drives `future` to completion without letting the clock move.
        ///
        /// Asserts that it did not: a clock that moved here would be exactly
        /// the auto-advance these tests are built to rule out.
        async fn frozen<F: Future>(future: F) -> F::Output {
            let mut future = std::pin::pin!(future);
            let at = Instant::now();
            let deadline = std::time::Instant::now() + HUNG;

            loop {
                if let Poll::Ready(output) = futures::poll!(future.as_mut()) {
                    assert_eq!(Instant::now(), at, "the clock moved on its own");

                    return output;
                }

                assert!(
                    std::time::Instant::now() < deadline,
                    "nothing happened for {HUNG:?} of real time: the test is hung",
                );
                tokio::task::yield_now().await;
            }
        }

        /// The service identity every client here runs as.
        fn identity() -> ServiceIdentity {
            ServiceIdentity::new(ServiceName::parse("weather").unwrap())
        }

        /// A control client built exactly as a sidecar builds one.
        fn production(base: &str) -> ControlClient {
            ControlClient::new(base, &identity()).expect("a production control client")
        }

        /// A feed, and a count of the body bytes that have reached it.
        ///
        /// The count is what lets a test know a keep-alive comment has been
        /// *read* — the stream skips comments, so nothing else shows it — and
        /// it is taken outside `reqwest`'s body, so a byte counted here is one
        /// that has already reset the read timeout.
        struct Feed {
            stream: EventStream,
            received: Arc<AtomicUsize>,
        }

        impl Feed {
            /// Opens the feed through `control`, with the clock held.
            async fn open(control: &ControlClient) -> Self {
                let mut stream = frozen(control.events(None)).await.expect("the feed opens");
                let received = Arc::new(AtomicUsize::new(0));
                let counted = Arc::clone(&received);

                let body = std::mem::replace(&mut stream.body, Box::pin(futures::stream::empty()));
                stream.body = Box::pin(body.inspect(move |chunk| {
                    if let Ok(bytes) = chunk {
                        counted.fetch_add(bytes.len(), Ordering::SeqCst);
                    }
                }));

                Self { stream, received }
            }

            /// The next event's id, with the clock held.
            async fn next(&mut self) -> Option<u64> {
                frozen(self.stream.next()).await.map(|event| event.id)
            }

            /// Reads until `bytes` of body have arrived in all, insisting that
            /// the stream neither ends nor yields an event meanwhile.
            async fn read_through(&mut self, bytes: usize) {
                let at = Instant::now();
                let deadline = std::time::Instant::now() + HUNG;

                while self.received.load(Ordering::SeqCst) < bytes {
                    if let Poll::Ready(item) = futures::poll!(self.stream.next()) {
                        panic!(
                            "the feed should have stayed open and quiet, and it yielded {:?}",
                            item.map(|event| event.id),
                        );
                    }

                    assert!(
                        std::time::Instant::now() < deadline,
                        "the server's bytes never arrived: the test is hung",
                    );
                    tokio::task::yield_now().await;
                }

                assert_eq!(Instant::now(), at, "the clock moved on its own");
            }

            /// Whether the feed is still open, having given everything already
            /// due — a timer the clock has passed, most of all — the chance to
            /// happen.
            ///
            /// A timer fires in the runtime's own bookkeeping as soon as the
            /// clock is past it, not when some I/O completes, so a feed that
            /// should have been cut is seen to be cut here however slow the
            /// host: this is not a race against anything.
            async fn is_open(&mut self) -> bool {
                for _ in 0..16 {
                    if let Poll::Ready(item) = futures::poll!(self.stream.next()) {
                        assert!(item.is_none(), "an event nobody sent");

                        return false;
                    }

                    tokio::task::yield_now().await;
                }

                true
            }
        }

        /// An SSE server that writes what it is told, when it is told to.
        ///
        /// `wiremock` answers a body in one go, and what is under test here is
        /// a response written over time and then not written to at all — so
        /// this is HTTP by hand, on a plain socket. The body has no length and
        /// no chunking: an HTTP/1.1 response with neither runs until the
        /// connection closes, which is exactly what a feed is.
        ///
        /// Each delay is a `tokio::time` sleep, so the script runs on the
        /// test's clock: nothing is written until the test advances it. The
        /// connection is **never closed**, so the only thing that can end a
        /// stream reading from it is the client's own timeout.
        async fn sse_server(
            script: Vec<(Duration, String)>,
        ) -> (String, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("an ephemeral port");
            let address = listener.local_addr().expect("the bound address");

            let handle = tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };

                // Enough of the request to know it arrived; the path is the
                // only one this server serves.
                let mut request = [0u8; 1024];
                let _ = socket.read(&mut request).await;

                if socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n",
                    )
                    .await
                    .is_err()
                {
                    return;
                }

                for (after, body) in script {
                    tokio::time::sleep(after).await;

                    if socket.write_all(body.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                }

                // Held open, and silent: the client decides when this is over.
                std::future::pending::<()>().await;
            });

            (format!("http://{address}"), handle)
        }

        /// One event, in the SSE wire format.
        fn frame(id: u64) -> String {
            format!(
                "id: {id}\nevent: channel.changed\ndata: {{\"id\":{id},\"at\":\"2026-09-22T00:00:00.000Z\",\"type\":\"channel.changed\",\"username\":\"ada\"}}\n\n",
            )
        }

        #[tokio::test(start_paused = true)]
        async fn a_feed_that_goes_silent_is_ended_at_the_idle_timeout_and_not_before() {
            // Something has to notice a connection that is up and dead, since
            // nothing bounds the feed's total length. That is the read timeout:
            // no byte at all — not even a keep-alive comment — for
            // `FEED_IDLE_TIMEOUT`, and the stream ends so the caller reopens.
            let (base, server) = sse_server(vec![(Duration::ZERO, frame(3))]).await;
            let mut feed = Feed::open(&production(&base)).await;

            assert_eq!(feed.next().await, Some(3));

            advance(FEED_IDLE_TIMEOUT - TICK).await;
            assert!(
                feed.is_open().await,
                "a feed is not dead until the whole idle timeout has passed",
            );

            advance(TICK).await;
            assert!(
                !feed.is_open().await,
                "no byte for the idle timeout is a dead feed",
            );

            server.abort();
        }

        #[tokio::test(start_paused = true)]
        async fn keepalive_comments_hold_a_silent_feed_open_past_the_idle_timeout() {
            // The reason the idle timeout is three keep-alives and not one: an
            // installation where nothing happens for an hour still has a feed,
            // and a client that reopened it every twenty seconds would be the
            // bug the feed client fixed, wearing a different number.
            let mut script = vec![(KEEPALIVE, COMMENT.to_string()); 5];
            script.push((KEEPALIVE, frame(9)));
            let (base, server) = sse_server(script).await;
            let mut feed = Feed::open(&production(&base)).await;
            let opened = Instant::now();

            for sent in 1..=5 {
                advance(KEEPALIVE).await;
                feed.read_through(sent * COMMENT.len()).await;
            }

            assert!(
                Instant::now() - opened > FEED_IDLE_TIMEOUT,
                "the comments alone have to outlast the idle timeout for this to prove anything",
            );

            advance(KEEPALIVE).await;
            assert_eq!(
                feed.next().await,
                Some(9),
                "the feed stays open across two minutes of nothing but comments",
            );

            server.abort();
        }

        #[tokio::test(start_paused = true)]
        async fn a_feed_has_no_total_timeout_where_an_ordinary_call_would_be_cut() {
            // The production finding. `reqwest`'s `timeout` is a deadline on the
            // whole exchange, body included, so the client the heartbeat uses
            // cut every feed at thirty seconds — a reopening every 31s, two
            // sidecars, 74 server log lines in two and a half minutes, nothing
            // wrong.
            //
            // First the feed a sidecar opens: an hour of keep-alives and then
            // an event, which nothing along the way may have cut.
            let hour = 180;
            let mut script = vec![(KEEPALIVE, COMMENT.to_string()); hour];
            script.push((KEEPALIVE, frame(7)));
            let (base, server) = sse_server(script).await;
            let mut feed = Feed::open(&production(&base)).await;

            for sent in 1..=hour {
                advance(KEEPALIVE).await;
                feed.read_through(sent * COMMENT.len()).await;
            }

            advance(KEEPALIVE).await;
            assert_eq!(
                feed.next().await,
                Some(7),
                "an hour-old feed is still a feed"
            );

            server.abort();

            // And the other half, so that the first proves something: the same
            // kind of feed read through the ordinary client — which is what a
            // `ControlClient` with no feed client of its own falls back to — is
            // cut at `DEFAULT_TIMEOUT`, although a byte arrived ten seconds
            // before and that client has no read timeout to trip.
            let (base, server) = sse_server(vec![
                (KEEPALIVE, COMMENT.to_string()),
                (KEEPALIVE, frame(7)),
            ])
            .await;
            let ordinary = http::client(&identity(), Trust::Public, DEFAULT_TIMEOUT)
                .expect("the ordinary client");
            let control = ControlClient::with_http(&base, ordinary, &identity()).unwrap();
            let mut feed = Feed::open(&control).await;

            advance(KEEPALIVE).await;
            feed.read_through(COMMENT.len()).await;

            advance(DEFAULT_TIMEOUT - KEEPALIVE - TICK).await;
            assert!(feed.is_open().await, "not cut before the total timeout");

            advance(TICK).await;
            assert!(!feed.is_open().await, "a total timeout cuts the body");

            server.abort();
        }

        #[tokio::test(start_paused = true)]
        async fn opening_a_feed_is_bounded_by_the_connect_timeout() {
            // With no total deadline, the connect timeout is what stands between
            // a sidecar and a server that takes the connection and never
            // answers: here, one that accepts TCP and never says a word of TLS.
            // It has to be given up on at `CONNECT_TIMEOUT` — not at the idle
            // timeout, which is longer, and not never.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("an ephemeral port");
            let address = listener.local_addr().expect("the bound address");
            let (accepted, mut taken) = tokio::sync::oneshot::channel();

            let server = tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let _ = accepted.send(());

                // Reads the ClientHello and whatever follows, and answers none
                // of it.
                let mut sink = [0u8; 4096];
                while matches!(socket.read(&mut sink).await, Ok(read) if read > 0) {}
                std::future::pending::<()>().await;
            });

            let control = production(&format!("https://{address}"));
            let started = Instant::now();
            let mut opening = std::pin::pin!(control.events(None));

            // Until the connection has been taken, so that the connect timer is
            // certainly running from the instant the test holds.
            frozen(async {
                loop {
                    assert!(
                        futures::poll!(opening.as_mut()).is_pending(),
                        "the open ended before the server had even accepted",
                    );

                    if taken.try_recv().is_ok() {
                        return;
                    }

                    tokio::task::yield_now().await;
                }
            })
            .await;

            advance(CONNECT_TIMEOUT - TICK).await;
            for _ in 0..16 {
                assert!(
                    futures::poll!(opening.as_mut()).is_pending(),
                    "given up on before the connect timeout",
                );
                tokio::task::yield_now().await;
            }

            advance(TICK).await;
            let refused = frozen(opening).await;

            assert!(refused.is_err(), "a server that never answers is an error");
            assert_eq!(
                Instant::now() - started,
                CONNECT_TIMEOUT,
                "and the connect timeout is what ended it, well before the idle timeout",
            );
            assert!(CONNECT_TIMEOUT < FEED_IDLE_TIMEOUT);

            server.abort();
        }
    }

    #[tokio::test]
    async fn a_feed_that_is_refused_is_an_error_rather_than_an_empty_stream() {
        // An empty stream would make a misconfigured token look like a quiet
        // server, which is the failure that takes an afternoon to find.
        let server = MockServer::start().await;
        let identity = ServiceIdentity::new(ServiceName::parse("weather").unwrap());
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": "The server-event feed is for administrators and services.",
            })))
            .mount(&server)
            .await;

        let err = ControlClient::with_http(&server.uri(), reqwest::Client::new(), &identity)
            .unwrap()
            .events(None)
            .await
            .unwrap_err();

        assert!(
            err.description().contains("administrators and services"),
            "{err}"
        );
    }
}
