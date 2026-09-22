//! Staying connected across the things that end a connection.
//!
//! A TAK stream ends for reasons that are not faults: a server restart, a
//! phone changing network, a NAT forgetting a mapping, the 25-second keepalive
//! deciding a silent socket is dead. [`Reconnecting`] turns all of them into a
//! pause, so the loop a sidecar writes is the same loop whether or not the
//! server went away in the middle of it.
//!
//! ```no_run
//! use futures::StreamExt;
//! use rustak_client::stream::{Reconnecting, StreamConfig};
//!
//! # async fn example(config: StreamConfig) {
//! let mut stream = Reconnecting::new(config);
//!
//! while let Some(event) = stream.next().await {
//!     tracing::info!(uid = %event.uid, "A message arrived.");
//! }
//! # }
//! ```
//!
//! # The on-connect hook
//!
//! A reconnected client is a *new* subscription as far as the server is
//! concerned: it has no callsign, no clientUid and no latest SA until it sends
//! one. The hook runs after every successful connect, before any event is
//! delivered, and is where a sidecar re-sends the position or the subscription
//! it wants the server to know about. It takes the stream and gives it back,
//! rather than borrowing it, so that the future it returns owns everything it
//! needs and can be held across the reconnect.
//!
//! # What the counters mean
//!
//! [`attempts`](Reconnecting::attempts) counts connection attempts **since the
//! last successful one**, so it reads as "how many times in a row this has
//! failed" — a healthy client that has been up for a day reports zero, not the
//! number of times it has ever dialled. That lifetime figure is
//! [`connects`](Reconnecting::connects): how many times this process has had a
//! connection up, which is the number that tells an operator whether a stream
//! has been flapping.
//!
//! # It never ends
//!
//! The [`Stream`] implementation has no final `None`: a caller stops it by
//! dropping it, or by racing it against
//! [`Shutdown::cancelled`](rustak_core::runtime::Shutdown::cancelled). Errors
//! are logged and retried rather than surfaced, which is the whole point of
//! the wrapper.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{Sink, Stream};
use rustak_cot::Event;
use tokio::time::{Instant, Sleep, sleep_until};

use super::{StreamConfig, StreamError, TakStream};

/// The shortest pause between attempts.
pub const MIN_BACKOFF: Duration = Duration::from_secs(1);

/// The longest pause between attempts.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// What a failed attempt says before this process has ever been connected.
///
/// A sidecar that has never reached its server has a certificate, an address
/// or a firewall problem — not a stream that "went away", which is what the
/// first line of a plugin's log used to claim.
const NEVER_CONNECTED: &str = "Could not connect to the TAK stream yet; retrying.";

/// What a failed attempt says once there has been a connection to lose.
const WENT_AWAY: &str = "The TAK stream went away; reconnecting.";

/// What to run on every successful connect, before any event is delivered.
pub type ConnectHook =
    Arc<dyn Fn(TakStream) -> BoxFuture<'static, Result<TakStream, StreamError>> + Send + Sync>;

/// A connection that reopens itself.
pub struct Reconnecting {
    config: Arc<StreamConfig>,
    hook: Option<ConnectHook>,
    backoff: Duration,
    /// Attempts since the last successful connection.
    attempts: u64,
    /// Successful connections in the life of this process.
    connects: u64,
    last_error: Option<String>,
    state: State,
}

/// Where the wrapper is in the connect / run / retry cycle.
enum State {
    /// Nothing has been attempted yet.
    Cold,
    /// Waiting out the backoff.
    Waiting(Pin<Box<Sleep>>),
    /// The transport handshake is in flight.
    Connecting(BoxFuture<'static, Result<TakStream, StreamError>>),
    /// The on-connect hook is running.
    Preparing(BoxFuture<'static, Result<TakStream, StreamError>>),
    /// Connected.
    Live(Box<TakStream>),
}

/// One transition, decided while the state is borrowed and applied after.
enum Step {
    Dial,
    Prepare(TakStream),
    Live(TakStream),
    Failed(StreamError),
}

impl Reconnecting {
    /// A reconnecting connection to the server `config` names.
    pub fn new(config: StreamConfig) -> Self {
        Self {
            config: Arc::new(config),
            hook: None,
            backoff: MIN_BACKOFF,
            attempts: 0,
            connects: 0,
            last_error: None,
            state: State::Cold,
        }
    }

    /// Runs `hook` after every successful connect, before any event is
    /// delivered.
    #[must_use]
    pub fn with_hook(mut self, hook: ConnectHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// The connection, if there is one right now.
    #[must_use]
    pub fn stream(&self) -> Option<&TakStream> {
        match &self.state {
            State::Live(stream) => Some(stream),
            _ => None,
        }
    }

    /// Whether there is a connection right now.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.stream().is_some()
    }

    /// How many connection attempts have been made since the last successful
    /// one, successful or not.
    ///
    /// Reset by every successful connection, so this is the length of the
    /// current outage rather than a tally of the process's whole life. For
    /// that, see [`connects`](Self::connects).
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }

    /// How many times this process has had a connection up.
    ///
    /// Never reset. A number that keeps climbing on a connection that is
    /// supposed to be permanent is a stream that is flapping.
    #[must_use]
    pub const fn connects(&self) -> u64 {
        self.connects
    }

    /// The pause that would follow a failure right now.
    #[must_use]
    pub const fn backoff(&self) -> Duration {
        self.backoff
    }

    /// Why the last connection ended, or why the last attempt failed.
    ///
    /// The wrapper turns every failure into a retry, so this is the only way
    /// for a caller to say *what* it is retrying — a sidecar reporting an
    /// outage to its plugin, or a test asserting on the reason. `None` before
    /// anything has gone wrong.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Doubles the backoff, up to the ceiling, and returns what to wait.
    fn take_backoff(&mut self) -> Duration {
        let waiting = self.backoff;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);

        waiting
    }

    /// Ends the current connection, or gives up on an attempt, and schedules
    /// the next one.
    ///
    /// The wording is different before the first connection of the process has
    /// ever been up, because "the stream went away" is not what happened to a
    /// sidecar that has never reached its server: that one is a certificate,
    /// an address or a firewall, and an operator reading the log should be
    /// pointed at the right thing.
    fn drop_connection(&mut self, reason: &str) {
        let waiting = self.take_backoff();
        self.last_error = Some(reason.to_string());

        match self.connects {
            0 => tracing::warn!(
                reason,
                retry_in = ?waiting,
                attempts = self.attempts,
                endpoint = %self.config.endpoint,
                "{NEVER_CONNECTED}",
            ),
            _ => tracing::warn!(
                reason,
                retry_in = ?waiting,
                attempts = self.attempts,
                connects = self.connects,
                "{WENT_AWAY}",
            ),
        }

        self.state = State::Waiting(Box::pin(sleep_until(Instant::now() + waiting)));
    }

    /// Which of the two outage lines a failure would log right now.
    ///
    /// The branch is the thing worth testing and a `tracing` message is not
    /// readable from inside the process, so the decision lives here and the
    /// macros above log these very strings.
    #[cfg(test)]
    const fn outage_line(&self) -> &'static str {
        match self.connects {
            0 => NEVER_CONNECTED,
            _ => WENT_AWAY,
        }
    }

    /// Advances the cycle until there is a live connection.
    fn poll_connect(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            let step = match &mut self.state {
                State::Cold => Step::Dial,
                State::Waiting(sleep) => {
                    ready!(sleep.as_mut().poll(cx));
                    Step::Dial
                }
                State::Connecting(connecting) => match ready!(connecting.as_mut().poll(cx)) {
                    Ok(stream) => Step::Prepare(stream),
                    Err(error) => Step::Failed(error),
                },
                State::Preparing(preparing) => match ready!(preparing.as_mut().poll(cx)) {
                    Ok(stream) => Step::Live(stream),
                    Err(error) => Step::Failed(error),
                },
                State::Live(_) => return Poll::Ready(()),
            };

            self.apply(step);
        }
    }

    /// Applies a transition decided by [`poll_connect`](Self::poll_connect).
    fn apply(&mut self, step: Step) {
        self.state = match step {
            Step::Dial => {
                self.attempts = self.attempts.saturating_add(1);
                State::Connecting(Box::pin(dial(self.config.clone())))
            }
            Step::Prepare(stream) => match &self.hook {
                Some(hook) => State::Preparing(hook(stream)),
                None => self.live(stream),
            },
            Step::Live(stream) => self.live(stream),
            Step::Failed(error) => {
                self.drop_connection(&error.to_string());
                return;
            }
        };
    }

    /// Accepts a connection: the backoff starts again from the bottom, and so
    /// does the attempt counter.
    fn live(&mut self, stream: TakStream) -> State {
        self.backoff = MIN_BACKOFF;
        self.connects = self.connects.saturating_add(1);

        let took = self.attempts;
        // Zero from here until something fails, so that the next disconnection
        // reports the length of *that* outage rather than this process's whole
        // history.
        self.attempts = 0;

        tracing::info!(
            endpoint = %self.config.endpoint,
            attempts = took,
            connects = self.connects,
            "The TAK stream is connected.",
        );

        State::Live(Box::new(stream))
    }
}

/// Opens one connection, owning its configuration so the future is `'static`.
async fn dial(config: Arc<StreamConfig>) -> Result<TakStream, StreamError> {
    super::connect(&config).await
}

impl Stream for Reconnecting {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let this = self.get_mut();

        loop {
            ready!(this.poll_connect(cx));

            let State::Live(stream) = &mut this.state else {
                continue;
            };

            match ready!(Pin::new(stream.as_mut()).poll_next(cx)) {
                Some(Ok(event)) => return Poll::Ready(Some(event)),
                Some(Err(error)) => this.drop_connection(&error.to_string()),
                None => this.drop_connection("the server closed the connection"),
            }
        }
    }
}

impl Sink<Event> for Reconnecting {
    type Error = StreamError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        let this = self.get_mut();
        ready!(this.poll_connect(cx));

        match &mut this.state {
            State::Live(stream) => Pin::new(stream.as_mut()).poll_ready(cx),
            _ => Poll::Pending,
        }
    }

    fn start_send(self: Pin<&mut Self>, event: Event) -> Result<(), StreamError> {
        match &mut self.get_mut().state {
            State::Live(stream) => Pin::new(stream.as_mut()).start_send(event),
            _ => Err(StreamError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "the TAK stream is reconnecting",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        match &mut self.get_mut().state {
            State::Live(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            _ => Poll::Ready(Ok(())),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        match &mut self.get_mut().state {
            State::Live(stream) => Pin::new(stream.as_mut()).poll_close(cx),
            _ => Poll::Ready(Ok(())),
        }
    }
}

impl std::fmt::Debug for Reconnecting {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reconnecting")
            .field("endpoint", &self.config.endpoint)
            .field("connected", &self.is_connected())
            .field("attempts", &self.attempts)
            .field("connects", &self.connects)
            .field("backoff", &self.backoff)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::Endpoint;

    fn reconnecting() -> Reconnecting {
        Reconnecting::new(StreamConfig::new(
            Endpoint::tls("tak.example.com", 8089),
            "SERVICE-adsb",
        ))
    }

    #[test]
    fn the_backoff_doubles_from_one_second_to_a_thirty_second_ceiling() {
        // A client that retried every second forever would be a denial of
        // service against a server that is already struggling; one that backed
        // off past half a minute would take too long to notice it is back.
        let mut stream = reconnecting();

        let waited: Vec<_> = (0..8).map(|_| stream.take_backoff()).collect();

        assert_eq!(waited[0], MIN_BACKOFF);
        assert_eq!(waited[1], Duration::from_secs(2));
        assert_eq!(waited[2], Duration::from_secs(4));
        assert_eq!(*waited.last().unwrap(), MAX_BACKOFF);
        assert_eq!(stream.backoff(), MAX_BACKOFF);
    }

    #[tokio::test]
    async fn a_successful_connect_starts_the_backoff_again_from_the_bottom() {
        // Otherwise an hour-long outage leaves a client that waits 30 seconds
        // before reacting to the *next* blip, forever.
        let mut stream = reconnecting();
        stream.take_backoff();
        stream.take_backoff();
        assert_ne!(stream.backoff(), MIN_BACKOFF);

        stream.apply(Step::Failed(StreamError::RxTimeout));
        let recovered = stream.live(TakStream::over(tokio::io::duplex(64).0, "SERVICE-adsb"));

        assert!(matches!(recovered, State::Live(_)));
        assert_eq!(stream.backoff(), MIN_BACKOFF);
    }

    #[tokio::test]
    async fn a_successful_connection_clears_the_attempt_count_and_counts_itself() {
        // `attempts` used to count every dial for the life of the process, so
        // a sidecar that had been up for an hour reported `attempts=17` on a
        // reconnect and read like something that had failed seventeen times.
        let mut stream = reconnecting();

        stream.apply(Step::Dial);
        stream.apply(Step::Failed(StreamError::RxTimeout));
        stream.apply(Step::Dial);

        assert_eq!(stream.attempts(), 2, "two failures in a row so far");
        assert_eq!(stream.connects(), 0);

        stream.state = stream.live(TakStream::over(tokio::io::duplex(64).0, "SERVICE-adsb"));

        assert_eq!(
            stream.attempts(),
            0,
            "a connection that is up has not failed at anything",
        );
        assert_eq!(stream.connects(), 1, "and this process has connected once");
        assert!(format!("{stream:?}").contains("connects"));

        // The next outage starts counting from zero.
        stream.apply(Step::Failed(StreamError::RxTimeout));
        assert_eq!(stream.attempts(), 0);
        stream.apply(Step::Dial);
        assert_eq!(stream.attempts(), 1);
        assert_eq!(stream.connects(), 1, "the lifetime figure is not reset");
    }

    #[tokio::test]
    async fn a_client_that_has_never_connected_does_not_say_the_stream_went_away() {
        // The first line in a sidecar's log used to be "The TAK stream went
        // away; reconnecting." before it had ever been anywhere.
        let mut stream = reconnecting();

        assert_eq!(stream.outage_line(), NEVER_CONNECTED);

        stream.apply(Step::Dial);
        stream.apply(Step::Failed(StreamError::Timeout("the connection".into())));

        assert_eq!(
            stream.outage_line(),
            NEVER_CONNECTED,
            "still nothing to have gone away",
        );

        stream.state = stream.live(TakStream::over(tokio::io::duplex(64).0, "SERVICE-adsb"));
        stream.apply(Step::Failed(StreamError::RxTimeout));

        assert_eq!(
            stream.outage_line(),
            WENT_AWAY,
            "once there has been a connection, losing it is what happened",
        );
    }

    #[test]
    fn nothing_is_connected_before_the_first_attempt() {
        let stream = reconnecting();

        assert!(!stream.is_connected());
        assert!(stream.stream().is_none());
        assert_eq!(stream.attempts(), 0);
        assert!(format!("{stream:?}").contains("tak.example.com"));
    }

    #[test]
    fn a_send_without_a_connection_is_refused_rather_than_swallowed() {
        let mut stream = reconnecting();
        let event = rustak_cot::msgs::ping("SERVICE-adsb", rustak_cot::CotTime::now());

        let error = Pin::new(&mut stream)
            .start_send(event)
            .expect_err("there is no connection to send on");

        assert!(matches!(error, StreamError::Io(_)), "{error:?}");
        assert!(!stream.is_connected());
    }
}
