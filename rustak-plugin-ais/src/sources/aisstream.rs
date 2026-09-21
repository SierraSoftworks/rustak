//! AISStream.io: worldwide AIS over a WebSocket, for the price of a free key.
//!
//! The protocol is one sentence long: open `wss://stream.aisstream.io/v0/stream`,
//! send a JSON subscription naming your API key and your bounding boxes within
//! three seconds, and read messages until something goes wrong. Everything else
//! here is the "until something goes wrong" half — a task that reconnects with
//! a capped backoff, a buffer the sidecar's tick drains, and a connection state
//! the heartbeat reports.
//!
//! # The key
//!
//! It is held in a [`Secret`] from the settings file to the subscription
//! message, it is never a field on a log line, and the subscription — which
//! contains it — is a `Secret` too, so that a `Debug` of this source cannot be
//! the way it escapes. Errors reported on the heartbeat are built from the
//! endpoint and the transport, never from the payload.
//!
//! # What this does not do
//!
//! `permessage-deflate` is not negotiated: `tungstenite` does not implement the
//! extension, and no pure-Rust WebSocket client in the tree does. The stream is
//! JSON and compresses well, so an operator watching a large box over a metered
//! link should prefer a receiver of their own or a smaller area. Noted in the
//! M9-01 status note rather than silently skipped.
//!
//! **Terms:** a free API key from <https://aisstream.io/account>, and the terms
//! published on that site. No licence text is published for the data itself.

mod frames;
mod wire;

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rustak_client::feed::{Feed, Track};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::status::{Connection, ConnectionTx};
use crate::vessels::{Observation, Vessels};

use frames::{Frames, Step};

use super::{Backoff, SourceContext};

/// What this source calls itself in a log line and on a heartbeat.
pub const NAME: &str = "aisstream.io";

/// The one endpoint the service publishes.
const ENDPOINT: &str = "wss://stream.aisstream.io/v0/stream";

/// How many observations are buffered between the socket and the sidecar's
/// tick. A busy North Sea box is a few hundred messages a second and a tick is
/// seconds apart, so this is several ticks' worth of headroom; past it, the
/// oldest observation of a vessel is the one worth losing.
const BUFFER: usize = 8_192;

/// How long the handshake and the subscription are given.
///
/// A TCP connection into a black hole — a route that drops packets rather than
/// refusing them, which is what a captive network or a misconfigured egress
/// firewall looks like — never returns at all, and a source that waited for it
/// would sit on "never connected" for ever without retrying. Fifteen seconds is
/// generous for a handshake and short enough that the backoff still happens.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a silent socket is given before it is assumed dead.
///
/// The service sends nothing at all when nothing is moving in the subscribed
/// box, so this cannot be short. Fifteen minutes is long enough that a quiet
/// marina is not reconnected every tick, and short enough that a half-open TCP
/// connection — the failure no read ever notices — recovers on its own.
const IDLE_TIMEOUT: Duration = Duration::from_secs(900);

/// The message types a subscription asks for when the settings name none.
#[must_use]
pub fn default_message_types() -> Vec<String> {
    wire::MESSAGE_TYPES
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// A live subscription to AISStream.io.
#[derive(Debug)]
pub struct AisStream {
    /// What the socket task has heard since the last tick.
    observations: mpsc::Receiver<Observation>,
    /// The per-MMSI memory that turns two message types into one track.
    vessels: Vessels,
}

impl AisStream {
    /// Starts the socket task and answers the feed the plugin polls.
    ///
    /// Opening never fails: an upstream that is refusing connections is a
    /// heartbeat that says so, not a sidecar that will not start.
    #[must_use]
    pub fn open(api_key: &Secret, message_types: &[String], context: SourceContext) -> Self {
        // `rustls` picks its backend from crate features and panics when more
        // than one is enabled; rustak's graph enables two, transitively. Naming
        // the process default here is what lets `tokio-tungstenite` build a
        // client configuration of its own a few lines later.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let (sender, observations) = mpsc::channel(BUFFER);
        let subscription = Secret::new(wire::subscription(
            api_key.expose(),
            context.area,
            message_types,
        ));

        tokio::spawn(run(
            subscription,
            api_key.clone(),
            sender,
            context.connection,
            context.shutdown,
        ));

        Self {
            observations,
            vessels: Vessels::new(NAME, context.policy.stale(), context.policy.max_tracks),
        }
    }
}

#[async_trait]
impl Feed for AisStream {
    fn name(&self) -> &str {
        NAME
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        let mut observations = Vec::new();

        // Drains what has arrived and returns; the socket task is what waits.
        while let Ok(observation) = self.observations.try_recv() {
            observations.push(observation);
        }

        Ok(self.vessels.absorb_all(observations))
    }
}

/// Keeps the subscription open until the sidecar stops.
async fn run(
    subscription: Secret,
    api_key: Secret,
    sender: mpsc::Sender<Observation>,
    connection: ConnectionTx,
    shutdown: Shutdown,
) {
    let mut backoff = Backoff::default();
    let mut said: Option<String> = None;

    while !shutdown.is_cancelled() {
        match stream(&subscription, &api_key, &sender, &connection, &shutdown).await {
            Ok(()) => {
                info!(source = NAME, "The AIS stream closed; reconnecting.");
                backoff.reset();
                said = None;
            }
            Err(reason) => {
                // Only ever the endpoint, the transport, or the service's own
                // words with the key taken out of them: the subscription
                // payload, which holds the key, is never part of this.
                //
                // And only once per reason: a key the service will never
                // accept would otherwise write the same line every minute for
                // as long as the sidecar runs. The heartbeat keeps saying it.
                match said.as_deref() == Some(reason.as_str()) {
                    true => debug!(
                        source = NAME,
                        retry_in = ?backoff.next(),
                        "The AIS stream is still unavailable: {reason}",
                    ),
                    false => warn!(
                        source = NAME,
                        retry_in = ?backoff.next(),
                        "The AIS stream is unavailable: {reason}",
                    ),
                }

                said = Some(reason.clone());
                connection.send_replace(Connection::reconnecting(reason));
            }
        }

        if !backoff.wait(&shutdown).await {
            return;
        }
    }
}

/// One connection, from the handshake to whatever ended it.
async fn stream(
    subscription: &Secret,
    api_key: &Secret,
    sender: &mpsc::Sender<Observation>,
    connection: &ConnectionTx,
    shutdown: &Shutdown,
) -> Result<(), String> {
    let opened = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(ENDPOINT))
        .await
        .map_err(|_| format!("{ENDPOINT} did not answer within {CONNECT_TIMEOUT:?}"))?;
    let (mut socket, _) =
        opened.map_err(|err| format!("{ENDPOINT} refused the connection ({err})"))?;

    // Within three seconds of the socket opening, or the server closes it.
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        socket.send(Message::text(subscription.expose())),
    )
    .await
    .map_err(|_| "the subscription could not be sent in time".to_string())?
    .map_err(|err| format!("the subscription could not be sent ({err})"))?;

    info!(source = NAME, "Subscribed to the AIS stream.");
    connection.send_replace(Connection::connected());

    let mut frames = Frames::new(sender, connection, api_key);

    loop {
        let message = tokio::select! {
            biased;

            () = shutdown.cancelled() => return Ok(()),
            // One wake-up, two minutes in, so that a connection nothing ever
            // arrives on is still heard from. After it, the frames themselves
            // are what drive that check.
            () = tokio::time::sleep_until(frames.silent_at()), if frames.due() => {
                frames.check_silence();

                continue;
            }
            message = tokio::time::timeout(IDLE_TIMEOUT, socket.next()) => message,
        };

        let message = match message {
            Err(_) => return Err(format!("nothing arrived for {IDLE_TIMEOUT:?}")),
            Ok(None) => return Ok(()),
            Ok(Some(Err(err))) => return Err(format!("the stream failed ({err})")),
            Ok(Some(Ok(message))) => message,
        };

        match frames.accept(message)? {
            Step::Continue => {}
            Step::Closed => return Ok(()),
            Step::KeepAlive => {
                // The pong is queued by the protocol layer on read; flushing is
                // what actually puts it on the wire, and a server that never
                // gets one drops the connection.
                socket
                    .flush()
                    .await
                    .map_err(|err| format!("the keep-alive could not be answered ({err})"))?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::feed::{Area, PublishPolicy};

    fn context() -> (SourceContext, crate::status::ConnectionRx) {
        let (connection, state) = crate::status::connection();

        (
            SourceContext {
                area: Area::Bbox {
                    south: 51.7,
                    west: 3.6,
                    north: 52.3,
                    east: 4.7,
                },
                policy: PublishPolicy::default(),
                shutdown: Shutdown::new(),
                connection,
            },
            state,
        )
    }

    #[test]
    fn the_default_subscription_asks_for_positions_and_names() {
        assert_eq!(
            default_message_types(),
            vec![
                "PositionReport".to_string(),
                "ShipStaticData".to_string(),
                "StandardClassBPositionReport".to_string(),
                "ExtendedClassBPositionReport".to_string(),
            ],
        );
    }

    #[tokio::test]
    async fn an_upstream_that_is_down_is_a_heartbeat_rather_than_a_refusal_to_start() {
        // The whole point of the source owning its reconnection: `open` hands
        // back a feed whatever the internet is doing, and `poll` answers
        // nothing rather than an error until something arrives.
        //
        // The shutdown is cancelled *before* `open`, so the socket task exits
        // at the top of its loop: this suite reaches no upstream, and the
        // reconnection itself is covered by the backoff's own test.
        let (context, state) = context();
        context.shutdown.cancel();

        let mut feed =
            AisStream::open(&Secret::new("not-a-key"), &default_message_types(), context);

        assert_eq!(feed.name(), "aisstream.io");
        assert!(feed.poll().await.expect("a poll never fails").is_empty());
        assert_eq!(
            *state.borrow(),
            Connection::Waiting,
            "nothing connected, and nothing pretended to",
        );
    }

    #[tokio::test]
    async fn a_decoded_message_becomes_a_track_on_the_next_poll() {
        let (sender, observations) = mpsc::channel(8);
        let (source, _state) = crate::status::connection();
        let key = Secret::new("not-a-key");
        let mut frames = Frames::new(&sender, &source, &key);
        let mut feed = AisStream {
            observations,
            vessels: Vessels::new(NAME, Duration::from_secs(120), 100),
        };

        // The two halves of a vessel, in the order a receiver hears them — and
        // in the binary frames the service actually sends them in.
        for body in [
            r#"{"MessageType":"PositionReport",
                "MetaData":{"MMSI":244660000,"latitude":51.95,"longitude":4.13,
                            "time_utc":"2026-09-20 12:00:00 +0000 UTC"},
                "Message":{"PositionReport":{"Latitude":51.95,"Longitude":4.13,
                                             "Sog":6.2,"Cog":271.5,"NavigationalStatus":0}}}"#,
            r#"{"MessageType":"ShipStaticData",
                "MetaData":{"MMSI":244660000,"latitude":51.95,"longitude":4.13,
                            "time_utc":"2026-09-20 12:00:01 +0000 UTC"},
                "Message":{"ShipStaticData":{"Name":"ZEEBRUGGE","Type":70}}}"#,
        ] {
            frames
                .accept(Message::binary(body.as_bytes().to_vec()))
                .expect("a decodable message");
        }

        let tracks = feed.poll().await.expect("the poll drains the buffer");

        assert_eq!(tracks.len(), 2, "the position, then the same hull named");
        assert_eq!(tracks[0].callsign.as_deref(), Some("MMSI 244660000"));
        assert_eq!(tracks[1].callsign.as_deref(), Some("ZEEBRUGGE"));
        assert_eq!(
            tracks[1].kind,
            rustak_client::feed::TrackKind::Vessel(rustak_client::feed::VesselClass::Merchant),
            "the static report's ship type reaches the CoT symbol",
        );
        assert!(feed.poll().await.expect("an empty poll").is_empty());
    }

    #[test]
    fn the_process_names_a_crypto_provider_so_the_websocket_client_can_build_one() {
        // `tokio-tungstenite` builds a `ClientConfig` of its own when it is
        // given no connector, and `ClientConfig::builder()` panics in a
        // dependency graph with more than one `rustls` backend feature enabled
        // — which rustak's is, transitively. `AisStream::open` installs the
        // process default; this is the assertion that doing so is what stops
        // the panic, because the panic itself would otherwise only ever happen
        // against the live service.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        let _config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }

    #[test]
    fn the_subscription_holding_the_key_cannot_be_printed() {
        let subscription = Secret::new(wire::subscription(
            "rsk_supersecret",
            Area::default(),
            &default_message_types(),
        ));

        assert!(!format!("{subscription:?}").contains("rsk_supersecret"));
        assert!(
            subscription.expose().contains("rsk_supersecret"),
            "still sent"
        );
    }
}
