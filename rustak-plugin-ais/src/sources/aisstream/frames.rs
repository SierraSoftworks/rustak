//! One connection's frames, and what each of them turned out to be.
//!
//! AISStream sends its JSON in **binary** frames — the payload is UTF-8, the
//! opcode is not text — so a reader that only looks inside `Message::Text`
//! subscribes successfully and then tracks nothing at all, which is what the
//! first Dublin deployment did. Both kinds are read the same way here.
//!
//! Everything that is neither is *counted* rather than shrugged at, because
//! the other half of that morning was that nothing said so: a feed which
//! understands nothing arriving on it must never again look like a feed with
//! nothing to understand.

use std::time::Duration;

use rustak_core::prelude::*;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::status::ConnectionTx;
use crate::vessels::Observation;

use super::{NAME, wire};

/// How long a connection may decode nothing before it says so.
///
/// Two minutes: long enough that a quiet bounding box is never what triggers
/// it — Dublin Bay at night is about one position report every twenty seconds,
/// and the subscription confirmation alone is enough to keep this quiet — and
/// short enough that whoever is watching a first deployment hears about a
/// broken one while they are still watching.
const SILENT_AFTER: Duration = Duration::from_secs(120);

/// The message the service sends when it has accepted a subscription.
///
/// It arrives about 200 ms after subscribing, and it is the proof that the
/// key, the bounding box and this reader all work.
const CONFIRMATION: &str = "SubscriptionConfirmation";

/// What the caller should do with the socket after a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Read the next frame.
    Continue,

    /// Answer the server's keep-alive, then read the next frame.
    KeepAlive,

    /// The server closed the stream.
    Closed,
}

/// What one connection has read, and what it made of it.
pub struct Frames<'a> {
    /// Where an observation goes when one is decoded.
    sender: &'a mpsc::Sender<Observation>,

    /// The counters and the notice the heartbeat carries.
    connection: &'a ConnectionTx,

    /// The key — held only so that a server's error text cannot be the thing
    /// that prints it.
    key: &'a Secret,

    /// When this connection subscribed.
    since: Instant,

    /// Data frames seen, whatever became of them.
    frames: u64,

    /// Messages recognised, the subscription confirmation included.
    decoded: u64,

    /// Whether the confirmation has been logged.
    confirmed: bool,

    /// Whether the caller still needs to wake this up at [`Frames::silent_at`].
    due: bool,

    /// Whether a notice of this connection's is standing.
    noticed: bool,

    /// Whether "nothing arrived at all" has been said.
    said_nothing_arrived: bool,

    /// Whether "nothing could be decoded" has been said.
    said_nothing_decoded: bool,
}

impl<'a> Frames<'a> {
    /// Starts reading a connection that has just subscribed.
    pub fn new(
        sender: &'a mpsc::Sender<Observation>,
        connection: &'a ConnectionTx,
        key: &'a Secret,
    ) -> Self {
        // Whatever the last connection had to say about itself, this one has
        // not said it yet.
        connection.clear_notice();

        Self {
            sender,
            connection,
            key,
            since: Instant::now(),
            frames: 0,
            decoded: 0,
            confirmed: false,
            due: true,
            noticed: false,
            said_nothing_arrived: false,
            said_nothing_decoded: false,
        }
    }

    /// Reads one frame.
    ///
    /// # Errors
    ///
    /// Whatever ends the connection: the service refusing the subscription, or
    /// a sidecar that has stopped reading. Anything this source merely cannot
    /// use is counted and skipped, because one unreadable frame is not a
    /// reason to drop a working stream.
    pub fn accept(&mut self, message: Message) -> Result<Step, String> {
        match message {
            // The payload is UTF-8 JSON in both: the service sends binary, its
            // documented examples are text, and neither is worth a second
            // decoder.
            Message::Text(text) => {
                self.frames += 1;
                self.json(text.as_str())?;
            }
            Message::Binary(bytes) => {
                self.frames += 1;

                match std::str::from_utf8(&bytes) {
                    Ok(text) => self.json(text)?,
                    Err(_) => {
                        self.connection.frame_undecoded();
                        debug!(source = NAME, "A frame that was not UTF-8 at all.");
                    }
                }
            }
            Message::Ping(_) => return Ok(Step::KeepAlive),
            Message::Close(_) => return Ok(Step::Closed),
            Message::Pong(_) => return Ok(Step::Continue),
            // Only the low-level API produces one of these, and it is not
            // something this source can read.
            Message::Frame(_) => {
                self.frames += 1;
                self.connection.frame_undecoded();
            }
        }

        self.check_silence();

        Ok(Step::Continue)
    }

    /// When the caller should wake this up if no frame does it first.
    pub fn silent_at(&self) -> Instant {
        self.since + SILENT_AFTER
    }

    /// Whether that wake-up is still worth taking.
    pub fn due(&self) -> bool {
        self.due
    }

    /// Says, once each and only when it is true, that this connection is open
    /// and understanding nothing.
    ///
    /// Two different sentences, because they send an operator to two different
    /// places: nothing arriving at all is a key or a bounding box, and frames
    /// arriving that decode into nothing is this plugin against a service that
    /// has moved. Neither fires for a quiet sea: a subscription confirmation
    /// on its own is something decoded.
    pub fn check_silence(&mut self) {
        if self.since.elapsed() < SILENT_AFTER {
            return;
        }

        // Past the deadline there is nothing left to wake up for; from here on
        // the frames themselves are what drive this.
        self.due = false;

        if self.decoded > 0 {
            self.recovered();

            return;
        }

        let frames = self.frames;
        let dropped = self.connection.dropped();

        let notice = match frames {
            0 if self.said_nothing_arrived => return,
            0 => {
                self.said_nothing_arrived = true;

                format!(
                    "Subscribed to {NAME} two minutes ago and not one frame has arrived; \
                     check the API key and the bounding box.",
                )
            }
            _ if self.said_nothing_decoded => return,
            _ => {
                self.said_nothing_decoded = true;

                format!(
                    "Connected to {NAME} for two minutes and none of the {frames} frames that \
                     arrived could be decoded ({} undecoded, {} ignored).",
                    dropped.frames_undecoded, dropped.messages_ignored,
                )
            }
        };

        warn!(source = NAME, "{notice}");

        self.connection.notice(notice);
        self.noticed = true;
    }

    /// Takes a standing notice back, for a connection that started decoding.
    fn recovered(&mut self) {
        if !self.noticed {
            return;
        }

        self.noticed = false;
        self.connection.clear_notice();

        info!(source = NAME, "The stream is being decoded again.");
    }

    /// Reads one frame's payload.
    fn json(&mut self, text: &str) -> Result<(), String> {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            self.connection.frame_undecoded();
            debug!(source = NAME, "A frame this source could not read as JSON.");

            return Ok(());
        };

        if let Some(refusal) = self.refusal(&value) {
            return Err(refusal);
        }

        if value.get("MessageType").and_then(Value::as_str) == Some(CONFIRMATION) {
            self.decoded += 1;

            if !self.confirmed {
                self.confirmed = true;
                info!(source = NAME, "AISStream confirmed the subscription.");
            }

            return Ok(());
        }

        let Ok(envelope) = serde_json::from_value::<wire::Envelope>(value) else {
            self.connection.message_ignored();
            debug!(source = NAME, "A message this source does not decode.");

            return Ok(());
        };

        self.decoded += 1;

        match envelope.observation() {
            Some(observation) => self.send(observation),
            None => Ok(()),
        }
    }

    /// The service's own words when it refuses — an object with an `error`
    /// key, whatever case it spells it in — and never this plugin's key,
    /// whatever words it is wrapped in.
    fn refusal(&self, value: &Value) -> Option<String> {
        let (_, reason) = value
            .as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("error"))?;
        let text = reason
            .as_str()
            .map_or_else(|| reason.to_string(), ToString::to_string);

        Some(format!(
            "the service refused the subscription ({})",
            self.redacted(&text),
        ))
    }

    /// The same text with the API key taken out of it.
    fn redacted(&self, text: &str) -> String {
        match self.key.is_empty() {
            // Replacing an empty needle would put the marker between every
            // character of the message.
            true => text.to_string(),
            false => text.replace(self.key.expose(), "***"),
        }
    }

    /// Hands an observation to the sidecar's tick.
    fn send(&self, observation: Observation) -> Result<(), String> {
        match self.sender.try_send(observation) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!(
                    source = NAME,
                    "The buffer is full; an observation was dropped."
                );

                Ok(())
            }
            // The plugin has gone; so should this task.
            Err(mpsc::error::TrySendError::Closed(_)) => Err("the sidecar stopped reading".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{ConnectionRx, Dropped, connection};

    /// What the service sends 200 ms after a subscription it accepted.
    const CONFIRMED: &str = r#"{"MessageType": "SubscriptionConfirmation"}"#;

    /// Written by hand from the documented shape, as every fixture in this
    /// repository is: no captured traffic is copied into rustak.
    fn position(mmsi: u32) -> Vec<u8> {
        format!(
            r#"{{"MessageType": "PositionReport",
                 "MetaData": {{"MMSI": {mmsi}, "latitude": 53.3406, "longitude": -6.2094,
                               "time_utc": "2026-09-21 21:00:00 +0000 UTC"}},
                 "Message": {{"PositionReport": {{"Latitude": 53.3406, "Longitude": -6.2094,
                                                  "Sog": 8.4, "Cog": 91.0,
                                                  "NavigationalStatus": 0, "Valid": true}}}}}}"#,
        )
        .into_bytes()
    }

    /// The pieces a connection is read with, in one place for a `Frames` to
    /// borrow from field by field.
    struct Parts {
        sender: mpsc::Sender<Observation>,
        observations: mpsc::Receiver<Observation>,
        source: ConnectionTx,
        state: ConnectionRx,
        key: Secret,
    }

    impl Parts {
        fn new() -> Self {
            let (sender, observations) = mpsc::channel(8);
            let (source, state) = connection();

            Self {
                sender,
                observations,
                source,
                state,
                key: Secret::new("rsk_supersecret"),
            }
        }
    }

    #[test]
    fn a_binary_frame_and_a_text_frame_carry_the_same_json_and_both_become_observations() {
        // The whole of the Dublin bug: every one of these arrived, and only
        // the text one was ever looked inside.
        let mut parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        assert_eq!(
            frames
                .accept(Message::binary(position(244_660_000)))
                .expect("a binary frame is read"),
            Step::Continue,
        );
        assert_eq!(
            frames
                .accept(Message::text(
                    String::from_utf8(position(244_660_001)).expect("UTF-8"),
                ))
                .expect("a text frame is read"),
            Step::Continue,
        );

        for mmsi in [244_660_000, 244_660_001] {
            let Ok(Observation::Position(position)) = parts.observations.try_recv() else {
                panic!("{mmsi} arrived as a position");
            };

            assert_eq!(position.mmsi, mmsi);
            assert_eq!(position.sog_knots, Some(8.4));
        }

        assert_eq!(
            parts.state.borrow().dropped,
            Dropped::default(),
            "nothing was dropped getting there",
        );
    }

    #[test]
    fn the_subscription_confirmation_is_a_message_this_source_knows() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        frames
            .accept(Message::binary(CONFIRMED.as_bytes().to_vec()))
            .expect("a confirmation is not a failure");

        assert_eq!(
            parts.state.borrow().dropped,
            Dropped::default(),
            "a confirmation is neither undecoded nor ignored",
        );
    }

    #[test]
    fn an_error_reply_ends_the_connection_and_never_carries_the_key() {
        for reply in [
            r#"{"error": "Api Key rsk_supersecret Is Not Valid"}"#,
            r#"{"Error": "Api Key rsk_supersecret Is Not Valid"}"#,
        ] {
            let parts = Parts::new();
            let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

            let refusal = frames
                .accept(Message::binary(reply.as_bytes().to_vec()))
                .expect_err("a refusal is the end of this connection");

            assert!(refusal.contains("Api Key"), "the server's words: {refusal}");
            assert!(refusal.contains("Is Not Valid"), "{refusal}");
            assert!(
                !refusal.contains("rsk_supersecret"),
                "and never the key: {refusal}",
            );
        }
    }

    #[test]
    fn a_frame_this_source_cannot_read_is_counted_rather_than_fatal() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        // A lone continuation byte: valid bytes, not valid UTF-8.
        assert_eq!(
            frames
                .accept(Message::binary(vec![0xff, 0xfe, 0x80]))
                .expect("not fatal"),
            Step::Continue,
        );
        assert_eq!(
            frames
                .accept(Message::text("not json".to_string()))
                .expect("not fatal"),
            Step::Continue,
        );

        assert_eq!(parts.state.borrow().dropped.frames_undecoded, 2);
        assert_eq!(parts.state.borrow().dropped.messages_ignored, 0);
    }

    #[test]
    fn a_message_this_source_does_not_recognise_is_counted_as_ignored() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        frames
            .accept(Message::binary(
                br#"{"MessageType": "AidsToNavigationReport", "Message": {}}"#.to_vec(),
            ))
            .expect("not fatal");

        assert_eq!(parts.state.borrow().dropped.messages_ignored, 1);
        assert_eq!(parts.state.borrow().dropped.frames_undecoded, 0);
    }

    #[test]
    fn a_keep_alive_and_a_close_are_answered_rather_than_decoded() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        assert_eq!(
            frames
                .accept(Message::Ping(Vec::new().into()))
                .expect("a ping"),
            Step::KeepAlive,
        );
        assert_eq!(
            frames.accept(Message::Close(None)).expect("a close"),
            Step::Closed,
        );
        assert_eq!(
            parts.state.borrow().dropped,
            Dropped::default(),
            "protocol frames are not dropped data",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_decodes_nothing_for_two_minutes_says_so_once() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        frames
            .accept(Message::binary(vec![0xff]))
            .expect("not fatal");

        assert_eq!(
            parts.state.borrow().notice,
            None,
            "not yet: it has been no time"
        );

        tokio::time::advance(Duration::from_secs(121)).await;

        frames
            .accept(Message::binary(vec![0xff]))
            .expect("not fatal");

        let notice = parts
            .state
            .borrow()
            .notice
            .expect("a connection that said so");

        assert!(notice.contains("could be decoded"), "{notice}");
        assert!(
            notice.contains("2 undecoded"),
            "with the counters: {notice}"
        );

        // Said once per connection, not once per frame from here to the end of
        // the deployment.
        parts.source.clear_notice();
        frames
            .accept(Message::binary(vec![0xff]))
            .expect("not fatal");

        assert_eq!(parts.state.borrow().notice, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_hears_nothing_at_all_says_so_once() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        assert!(frames.due(), "the caller has a wake-up to take");

        tokio::time::advance(Duration::from_secs(121)).await;
        frames.check_silence();

        let notice = parts
            .state
            .borrow()
            .notice
            .expect("a connection that said so");

        assert!(notice.contains("not one frame"), "{notice}");
        assert!(notice.contains("check the API key"), "{notice}");
        assert!(!frames.due(), "and nothing left to wake up for");

        parts.source.clear_notice();
        frames.check_silence();

        assert_eq!(parts.state.borrow().notice, None, "once, not every wake-up");
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_box_that_confirmed_its_subscription_never_warns() {
        // Dublin Bay at night: one report every twenty seconds, and minutes of
        // nothing in between. This must never be a warning.
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        frames
            .accept(Message::binary(CONFIRMED.as_bytes().to_vec()))
            .expect("a confirmation");

        tokio::time::advance(Duration::from_secs(600)).await;
        frames.check_silence();

        assert_eq!(
            parts.state.borrow().notice,
            None,
            "a confirmation is a pipeline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_notice_is_taken_back_when_the_stream_starts_decoding() {
        let parts = Parts::new();
        let mut frames = Frames::new(&parts.sender, &parts.source, &parts.key);

        tokio::time::advance(Duration::from_secs(121)).await;
        frames.check_silence();

        assert!(parts.state.borrow().notice.is_some());

        frames
            .accept(Message::binary(position(244_660_000)))
            .expect("a position report");

        assert_eq!(
            parts.state.borrow().notice,
            None,
            "the Services page stops saying it the moment it stops being true",
        );
    }
}
