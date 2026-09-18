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
        let mut request = self.request(reqwest::Method::GET, "/events");

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
