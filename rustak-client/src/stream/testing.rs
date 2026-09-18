//! A fake EUD, for the tests that need a real client on the other end.
//!
//! [`Eud`] is what `rustak-server`'s integration tests connect with, and what
//! the interop suites compare a real ATAK against. It is deliberately thin: a
//! [`TakStream`], a uid, a callsign, and the four operations an assertion is
//! written out of.
//!
//! ```no_run
//! use std::time::Duration;
//! use rustak_client::stream::testing::Eud;
//! use rustak_client::stream::{Endpoint, StreamConfig};
//!
//! # async fn example(config: StreamConfig) -> Result<(), rustak_client::stream::StreamError> {
//! let mut alpha = Eud::connect(&config, "ALPHA").await?;
//! alpha.send_sa(51.5074, -0.1278).await?;
//!
//! let seen = alpha
//!     .expect(|event| event.callsign() == Some("BRAVO"), Duration::from_secs(2))
//!     .await?;
//! assert_eq!(seen.point.lat, 51.5);
//! # Ok(()) }
//! ```
//!
//! Available behind the `testing` feature, which `rustak-server` takes as a
//! dev-dependency (`rustak-client = { workspace = true, features = ["testing"] }`).

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rustak_cot::Event;
use rustak_cot::detail::{Contact, Group, Takv, contact::STREAMING_ENDPOINT};

use super::{AsyncIo, StreamConfig, StreamError, TakStream, connect};

/// How long an SA message this helper sends stays fresh.
pub const SA_VALIDITY: Duration = Duration::from_secs(120);

/// The CoT type a friendly ground unit reports itself as.
pub const SA_TYPE: &str = "a-f-G-U-C";

/// How long [`Eud::send`] gives the connection to go quiet before carrying on.
pub const SETTLE_BUDGET: Duration = Duration::from_millis(250);

/// A client standing in for an end-user device.
#[derive(Debug)]
pub struct Eud {
    stream: TakStream,
    uid: String,
    callsign: String,
    team: String,
    role: String,
}

impl Eud {
    /// Connects a fake EUD with the given callsign.
    ///
    /// # Errors
    ///
    /// Whatever [`connect`] returns.
    pub async fn connect(
        config: &StreamConfig,
        callsign: impl Into<String>,
    ) -> Result<Self, StreamError> {
        let stream = connect(config).await?;

        Ok(Self::new(stream, callsign))
    }

    /// Wraps a stream that is already connected.
    pub fn new(stream: TakStream, callsign: impl Into<String>) -> Self {
        Self {
            uid: stream.uid().to_string(),
            stream,
            callsign: callsign.into(),
            team: "Cyan".to_string(),
            role: "Team Member".to_string(),
        }
    }

    /// Wraps any transport — a [`tokio::io::duplex`] half, most usefully.
    pub fn over(
        io: impl AsyncIo + 'static,
        uid: impl Into<String>,
        callsign: impl Into<String>,
    ) -> Self {
        Self::new(TakStream::over(io, uid), callsign)
    }

    /// Reports a different team and role in this EUD's SA messages.
    #[must_use]
    pub fn with_team(mut self, name: impl Into<String>, role: impl Into<String>) -> Self {
        self.team = name.into();
        self.role = role.into();
        self
    }

    /// This EUD's `clientUid`.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// This EUD's callsign.
    #[must_use]
    pub fn callsign(&self) -> &str {
        &self.callsign
    }

    /// The connection underneath, for assertions about the protocol itself.
    #[must_use]
    pub fn stream(&self) -> &TakStream {
        &self.stream
    }

    /// The connection underneath, mutably.
    pub fn stream_mut(&mut self) -> &mut TakStream {
        &mut self.stream
    }

    /// Builds the situational-awareness message this EUD would report.
    ///
    /// The shape is the one the server reads a subscription's identity out of
    /// (`compat/streaming.md` §10): a uid, a callsign, the streaming endpoint
    /// sentinel, and a `<__group>`.
    #[must_use]
    pub fn sa(&self, lat: f64, lon: f64) -> Event {
        Event::builder(SA_TYPE, self.uid.clone())
            .how("m-g")
            .point(lat, lon)
            .stale_after(SA_VALIDITY)
            .typed(&Contact::new(self.callsign.clone()).with_endpoint(STREAMING_ENDPOINT))
            .typed(&Group::new(self.team.clone(), self.role.clone()))
            .typed(&Takv {
                device: "rustak-client".to_string(),
                platform: "rustak".to_string(),
                os: std::env::consts::OS.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                extra: Vec::new(),
            })
            .build()
    }

    /// Sends one event.
    ///
    /// # Errors
    ///
    /// Whatever the connection returns. Note that an event sent while protocol
    /// negotiation is outstanding is *buffered*, not written — see
    /// [`TakStream::queued`].
    pub async fn send(&mut self, event: Event) -> Result<(), StreamError> {
        self.stream.send(event).await?;

        // A test sends and then asserts on somebody *else*, so the connection
        // has to be given the chance to finish its own protocol work here
        // rather than on the next read that may never come.
        self.stream.settle(SETTLE_BUDGET).await
    }

    /// Sends this EUD's SA message for a position.
    ///
    /// # Errors
    ///
    /// Whatever the connection returns.
    pub async fn send_sa(&mut self, lat: f64, lon: f64) -> Result<(), StreamError> {
        let event = self.sa(lat, lon);

        self.send(event).await
    }

    /// Waits for an event matching `predicate`, discarding the ones that do not.
    ///
    /// # Errors
    ///
    /// [`StreamError::Timeout`] when nothing matching arrives in time, and
    /// whatever the connection returns when it fails first.
    pub async fn expect(
        &mut self,
        mut predicate: impl FnMut(&Event) -> bool,
        timeout: Duration,
    ) -> Result<Event, StreamError> {
        let matching = async {
            while let Some(event) = self.stream.next().await {
                let event = event?;

                if predicate(&event) {
                    return Ok(event);
                }

                tracing::debug!(uid = %event.uid, r#type = %event.r#type, "Not what this EUD is waiting for.");
            }

            Err(StreamError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the connection closed while waiting",
            )))
        };

        tokio::time::timeout(timeout, matching).await.map_err(|_| {
            StreamError::Timeout(format!("an event {} was waiting for", self.callsign))
        })?
    }

    /// Waits for an event with the given uid.
    ///
    /// # Errors
    ///
    /// As [`expect`](Self::expect).
    pub async fn expect_uid(&mut self, uid: &str, timeout: Duration) -> Result<Event, StreamError> {
        let wanted = uid.to_string();

        self.expect(move |event| event.uid == wanted, timeout).await
    }

    /// Asserts that nothing arrives within `timeout`.
    ///
    /// This is the shape of every negative routing test — "B must not see A" —
    /// so it has to be the cheap thing to write, and it has to fail with the
    /// message that leaked rather than with a bare assertion.
    ///
    /// # Errors
    ///
    /// [`StreamError::Unexpected`] naming the event that arrived, and whatever
    /// the connection returns when it fails.
    pub async fn expect_none(&mut self, timeout: Duration) -> Result<(), StreamError> {
        match tokio::time::timeout(timeout, self.stream.next()).await {
            Err(_) => Ok(()),
            Ok(None) => Ok(()),
            Ok(Some(Err(error))) => Err(error),
            Ok(Some(Ok(event))) => Err(StreamError::Unexpected(format!(
                "a '{}' from {}",
                event.r#type, event.uid
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_cot::detail::TypedDetail;

    fn eud() -> Eud {
        Eud::over(tokio::io::duplex(1024).0, "ANDROID-1", "ALPHA")
    }

    #[tokio::test]
    async fn an_sa_message_carries_everything_a_subscription_is_read_from() {
        // `compat/streaming.md` §10: uid, callsign and `contact/@endpoint` are
        // what fix a subscription's identity, and `__group` is what routing
        // then depends on. An SA message missing any of them is one the server
        // will not learn anything from.
        let event = eud().with_team("Blue", "Team Lead").sa(51.5074, -0.1278);

        assert_eq!(event.uid, "ANDROID-1");
        assert_eq!(event.r#type, SA_TYPE);
        assert_eq!(event.callsign(), Some("ALPHA"));
        assert_eq!(event.endpoint(), Some(STREAMING_ENDPOINT));
        assert!(event.is_sa());

        let group = event.group().expect("an SA message carries its group");
        assert_eq!(group.name, "Blue");
        assert_eq!(group.role, "Team Lead");

        let takv = event.takv().expect("an SA message says what it is running");
        assert_eq!(takv.platform, "rustak");
    }

    #[tokio::test]
    async fn an_sa_message_goes_stale_rather_than_living_forever() {
        let event = eud().sa(1.0, 2.0);

        assert_eq!(event.stale - event.time, SA_VALIDITY.as_millis() as i64);
        assert!(!event.is_stale_at(event.time));
        assert!(event.is_stale_at(event.stale));
    }

    #[tokio::test(start_paused = true)]
    async fn expect_none_passes_on_silence_and_names_what_leaked() {
        let (mine, theirs) = tokio::io::duplex(4096);
        let mut eud = Eud::over(mine, "ANDROID-1", "ALPHA");
        let mut peer = Eud::over(theirs, "ANDROID-2", "BRAVO");

        eud.expect_none(Duration::from_millis(100))
            .await
            .expect("nothing has been sent");

        peer.send_sa(1.0, 2.0).await.expect("the peer can write");

        let error = eud
            .expect_none(Duration::from_millis(100))
            .await
            .expect_err("something arrived");

        assert!(error.to_string().contains("ANDROID-2"), "{error}");
        assert!(matches!(error, StreamError::Unexpected(_)), "{error:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn expect_skips_what_it_is_not_waiting_for() {
        let (mine, theirs) = tokio::io::duplex(8192);
        let mut eud = Eud::over(mine, "ANDROID-1", "ALPHA");
        let mut peer = Eud::over(theirs, "ANDROID-2", "BRAVO");

        peer.send_sa(1.0, 2.0).await.unwrap();
        peer.send(
            Event::builder("b-t-f", "CHAT-1")
                .point(3.0, 4.0)
                .push(Contact::new("BRAVO").to_element())
                .build(),
        )
        .await
        .unwrap();

        let chat = eud
            .expect(|event| event.r#type == "b-t-f", Duration::from_secs(1))
            .await
            .expect("the chat message arrives after the SA one");

        assert_eq!(chat.uid, "CHAT-1");
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_names_the_eud_that_was_waiting() {
        // The peer is held open: a *closed* connection is a different failure,
        // and one a routing test must not mistake for "nothing arrived".
        let (mine, _peer) = tokio::io::duplex(1024);
        let mut eud = Eud::over(mine, "ANDROID-1", "ALPHA");

        let error = eud
            .expect(|_| true, Duration::from_millis(50))
            .await
            .expect_err("nothing is ever sent");

        assert!(error.to_string().contains("ALPHA"), "{error}");
        assert!(matches!(error, StreamError::Timeout(_)), "{error:?}");
    }
}
