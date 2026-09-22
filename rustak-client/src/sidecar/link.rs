//! The sidecar's end of the CoT stream.
//!
//! [`Link`] is the one piece of glue between the harness and
//! [`crate::stream`]: it turns a sidecar's `[service]` and `[server]` sections
//! into a [`StreamConfig`], keeps a [`Reconnecting`] connection alive behind
//! it, and reports what that connection does as [`SidecarEvent`]s.
//!
//! # Why the events are derived rather than emitted
//!
//! [`Reconnecting`] deliberately hides outages: it logs them, backs off and
//! reopens, and its [`Stream`] yields nothing but successfully decoded events.
//! That is exactly right for the loop a plugin writes, and leaves nobody to
//! announce a reconnection to a plugin that *does* care — a sidecar has to
//! re-send its situational-awareness event on every new connection, because the
//! server treats it as a new subscription.
//!
//! So [`Link`] reads the transitions back out of the connection's own state
//! after each poll (`is_connected`, `last_error`, the stream's
//! [`Negotiation`](crate::stream::Negotiation)) and hands them to the plugin.
//! Nothing here drives the connection on its own: the state it reads is the
//! state the poll it just made left behind.
//!
//! # A sidecar with no stream
//!
//! `[server] stream` is optional — a plugin that only talks to the Marti API
//! has no use for a socket — so a [`Link`] without one is an idle stream that
//! is never ready, and publishing into it is a logged no-op. That keeps one
//! loop in [`run`](super::run) rather than two.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{SinkExt, Stream, StreamExt};
use rustak_core::prelude::*;
use rustak_cot::Event;

use super::link_health::{REMIND_EVERY, humanised};
use super::{SidecarContext, SidecarEvent};
use crate::stream::{Endpoint, Mode, Reconnecting, StreamConfig, TlsIdentity};

/// What is reported when a connection ends without the wrapper saying why.
const UNEXPLAINED: &str = "the connection ended";

/// How long [`Link::settle`] waits for the very first connection.
///
/// The first tick of a feed plugin happens the instant the sidecar starts, and
/// the first connection takes a handshake — tens of milliseconds in the
/// deployment this was found in. Without this wait the first batch was
/// published into a connection that was still being made and thrown away, with
/// a `warn` on every clean start that called a first connection a
/// reconnection.
///
/// Bounded, because a server that is not there must not stop a sidecar
/// starting: past this the ordinary path takes over, and the next tick
/// republishes.
pub(crate) const FIRST_CONNECT: Duration = Duration::from_secs(10);

/// The sidecar's connection to the CoT stream, and the bookkeeping that turns
/// its state changes into [`SidecarEvent`]s.
pub(crate) struct Link {
    /// The connection, or `None` for a sidecar with no `[server] stream`.
    connection: Option<Reconnecting>,

    /// What was dialled, as [`SidecarEvent::Connected`] reports it.
    endpoint: String,

    /// Whether the last poll left the connection up, so that a change is a
    /// [`SidecarEvent`] rather than a repeat.
    connected: bool,

    /// Whether the current connection's encoding has already been reported.
    negotiated: bool,

    /// An event the connection produced while a transition was still owed to
    /// the plugin. Held rather than dropped, because it arrived first.
    held: Option<Box<Event>>,

    /// What [`settle`](Self::settle) drained while it waited for the first
    /// connection, handed out before anything newer.
    queued: VecDeque<SidecarEvent>,

    /// Whether a publish into a connection that is not up is worth saying
    /// anything about, and at what level.
    discards: Discards,
}

/// Whether the stream has ever been up, and what a discarded batch is worth
/// saying.
///
/// The distinction is the point. Before the first connection there is nothing
/// wrong: the handshake is in flight, the plugin has simply produced its first
/// batch first, and "the CoT stream is reconnecting" is not true of a stream
/// that has never connected. After the stream has been up and dropped, the
/// discards are real and the first of them is worth a `warn` — once, with the
/// count, in [`link_health`](super::link_health)'s style rather than one line
/// per tick.
#[derive(Debug, Default)]
struct Discards {
    /// Whether the connection has ever come up.
    ever_connected: bool,

    /// Whether this run of discards has been announced.
    announced: bool,

    /// Events thrown away since the run was last mentioned.
    dropped: usize,

    /// When the run was last mentioned.
    mentioned_at: Option<DateTime<Utc>>,
}

/// What one discarded batch is worth saying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Discarded {
    /// The stream has never connected: there is nothing wrong yet.
    Starting,

    /// The stream was up and dropped, and this is the first batch lost to it.
    First { count: usize },

    /// Still down. Nothing new to say.
    Quiet,

    /// Still down, and it has been long enough to say how much has been lost.
    Reminder { count: usize, failing_for: Duration },
}

impl Discards {
    /// Records what the connection's state is, so a drop is a state change.
    fn connected(&mut self, connected: bool, now: DateTime<Utc>) {
        if connected {
            self.ever_connected = true;
            self.announced = false;
            self.dropped = 0;
            self.mentioned_at = None;
        } else if self.ever_connected && self.mentioned_at.is_none() {
            self.mentioned_at = Some(now);
        }
    }

    /// Records a discarded batch and answers what to say about it.
    fn discarded(&mut self, count: usize, now: DateTime<Utc>) -> Discarded {
        if !self.ever_connected {
            return Discarded::Starting;
        }

        self.dropped = self.dropped.saturating_add(count);

        if !self.announced {
            self.announced = true;
            self.mentioned_at = Some(now);

            return Discarded::First {
                count: self.dropped,
            };
        }

        let since = self.mentioned_at.unwrap_or(now);
        let quiet_for = (now - since).to_std().unwrap_or_default();

        if quiet_for < REMIND_EVERY {
            return Discarded::Quiet;
        }

        let count = std::mem::take(&mut self.dropped);
        self.mentioned_at = Some(now);

        Discarded::Reminder {
            count,
            failing_for: quiet_for,
        }
    }
}

impl Link {
    /// Builds the connection a context describes, without dialling it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when `[server] stream` is not a
    /// connect string this build can use, or when it names a TLS endpoint and
    /// `[service]` does not carry the certificate, key and truststore to open
    /// one with.
    pub(crate) fn open<S>(context: &SidecarContext<S>) -> Result<Self, Error> {
        let Some(configured) = context.config().server.stream.as_deref() else {
            tracing::info!(
                "This sidecar has no [server] stream, so it will not open a CoT connection.",
            );

            return Ok(Self::idle());
        };

        let config = stream_config(context, configured)?;
        let endpoint = config.endpoint.to_string();

        Ok(Self {
            connection: Some(Reconnecting::new(config)),
            endpoint,
            ..Self::idle()
        })
    }

    /// A link with nothing on the other end of it.
    fn idle() -> Self {
        Self {
            connection: None,
            endpoint: String::new(),
            connected: false,
            negotiated: false,
            held: None,
            queued: VecDeque::new(),
            discards: Discards::default(),
        }
    }

    /// Waits, bounded, for the connection to come up for the first time.
    ///
    /// Called once, before the first tick. A feed plugin's first batch is
    /// produced the instant the sidecar starts and the first connection takes a
    /// handshake, so without this the first batch is published into a socket
    /// that is not there yet and thrown away — which is what every clean start
    /// in the first live deployment did, with a `warn` to go with it.
    ///
    /// Whatever the connection produced while waiting is queued rather than
    /// consumed: the plugin is still told it connected, and in the same order.
    /// Answers whether the connection came up within `within`; `false` for a
    /// sidecar with no `[server] stream`, which has nothing to wait for.
    pub(crate) async fn settle(&mut self, within: Duration) -> bool {
        if self.connection.is_none() {
            return false;
        }

        let deadline = tokio::time::Instant::now() + within;
        let mut drained = Vec::new();

        let connected = loop {
            if self.connected {
                break true;
            }

            match tokio::time::timeout_at(deadline, self.next()).await {
                Ok(Some(event)) => drained.push(event),
                // The stream ended, or the wait ran out. Either way the
                // ordinary path takes it from here.
                Ok(None) | Err(_) => break self.connected,
            }
        };

        self.queued.extend(drained);

        if !connected {
            tracing::debug!(
                waited = %humanised(within),
                "The CoT stream has not connected yet; the first batch may be published into a connection that is still being made.",
            );
        }

        connected
    }

    /// The endpoint this link dials, for the start-up log line.
    pub(crate) fn endpoint(&self) -> Option<&str> {
        self.connection.as_ref().map(|_| self.endpoint.as_str())
    }

    /// Writes what a plugin returned, in the order it returned it.
    ///
    /// Events are dropped, with a log line counting them, when there is no
    /// connection to write them on. A position report held through a
    /// thirty-second backoff and then delivered is worse than one that was
    /// never sent, and a plugin that disagrees can return its own again from
    /// the next [`SidecarEvent::Connected`].
    ///
    /// # Errors
    ///
    /// Whatever the connection returns, rendered as an operator-facing error.
    pub(crate) async fn publish(&mut self, events: Vec<Event>) -> Result<(), Error> {
        if events.is_empty() {
            return Ok(());
        }

        let count = events.len();
        let Some(connection) = &mut self.connection else {
            // Not a warning: this sidecar was configured without a stream, so
            // every tick would repeat it, and an operator who wanted one has a
            // `[server] stream` line to add rather than a fault to chase.
            tracing::debug!(
                count,
                "Discarding events: no [server] stream is configured."
            );

            return Ok(());
        };

        if !connection.is_connected() {
            self.discard(count);

            return Ok(());
        }

        for event in events {
            tracing::debug!(uid = %event.uid, r#type = %event.r#type, "Publishing.");
            connection.feed(event).await?;
        }

        connection.flush().await?;

        Ok(())
    }

    /// Says as much about a batch that could not be published as its state
    /// calls for.
    ///
    /// A stream that has never connected is not "reconnecting" and is not a
    /// fault: the handshake is in flight and the plugin was simply first. A
    /// stream that *was* up and dropped is worth one `warn`, and then one every
    /// five minutes carrying the count, rather than one per tick.
    fn discard(&mut self, count: usize) {
        match self.discards.discarded(count, Utc::now()) {
            Discarded::Starting => tracing::debug!(
                count,
                "Discarding events: the CoT stream has not finished connecting yet.",
            ),
            Discarded::First { count } => tracing::warn!(
                count,
                "Discarding events: the CoT stream is reconnecting. Further discards are logged at debug until it is back.",
            ),
            Discarded::Quiet => tracing::debug!(
                count,
                "Discarding events: the CoT stream is still reconnecting.",
            ),
            Discarded::Reminder { count, failing_for } => tracing::warn!(
                count,
                "The CoT stream has been reconnecting for {}; {count} events have been discarded since this was last reported.",
                humanised(failing_for),
            ),
        }
    }

    /// The one transition the connection's current state owes the plugin, if
    /// there is one. Called after every poll, and drained one per poll.
    fn transition(&mut self) -> Option<SidecarEvent> {
        let connection = self.connection.as_ref()?;
        let connected = connection.is_connected();

        if connected != self.connected {
            self.connected = connected;
            self.negotiated = false;
            self.discards.connected(connected, Utc::now());

            return Some(if connected {
                SidecarEvent::Connected {
                    endpoint: self.endpoint.clone(),
                }
            } else {
                SidecarEvent::Disconnected {
                    reason: connection.last_error().unwrap_or(UNEXPLAINED).to_string(),
                }
            });
        }

        // The encoding is only decided some way into a connection: the server
        // offers, we ask, and it answers — or it never offers at all and the
        // connection's own timer settles it on XML.
        let stream = connection.stream()?;
        if connected && !self.negotiated && stream.negotiation().is_settled() {
            self.negotiated = true;

            return Some(SidecarEvent::Negotiated {
                protobuf: stream.mode() == Mode::Proto,
            });
        }

        None
    }
}

impl Stream for Link {
    type Item = SidecarEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<SidecarEvent>> {
        let this = self.get_mut();

        loop {
            // Whatever `settle` drained while it waited for the first
            // connection, in the order the connection produced it.
            if let Some(event) = this.queued.pop_front() {
                return Poll::Ready(Some(event));
            }

            // What the connection did comes before what it carried, so that a
            // plugin has been told it is connected before the first event of
            // that connection reaches it.
            if let Some(event) = this.transition() {
                return Poll::Ready(Some(event));
            }

            if let Some(event) = this.held.take() {
                return Poll::Ready(Some(SidecarEvent::Cot(event)));
            }

            let Some(connection) = &mut this.connection else {
                return Poll::Pending;
            };

            match Pin::new(connection).poll_next(cx) {
                // Round again: the poll that produced this may also have
                // brought the connection up.
                Poll::Ready(Some(event)) => this.held = Some(Box::new(event)),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => {
                    return match this.transition() {
                        Some(event) => Poll::Ready(Some(event)),
                        None => Poll::Pending,
                    };
                }
            }
        }
    }
}

impl std::fmt::Debug for Link {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Link")
            .field("endpoint", &self.endpoint())
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

/// Reads a sidecar's configuration as a stream configuration.
///
/// The uid is the service's own `SERVICE-<name>`, which is what the server
/// keys a subscription on, and the callsign is the descriptor's display name,
/// which is what an operator sees on the map.
fn stream_config<S>(context: &SidecarContext<S>, configured: &str) -> Result<StreamConfig, Error> {
    let endpoint: Endpoint = configured.parse()?;
    let identity = context.identity();

    let mut config = StreamConfig::new(endpoint, identity.uid().as_str())
        .with_callsign(context.descriptor().display());

    // The certificate is the service's primary credential, and a TLS endpoint
    // cannot be opened without one — so the missing file is named here, rather
    // than as a handshake alert on the first connection attempt.
    if config.endpoint.tls {
        config = config.with_tls(TlsIdentity::from_identity(identity)?);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{NoSettings, SidecarConfig};
    use futures::StreamExt;

    fn context(text: &str) -> SidecarContext<NoSettings> {
        let config: SidecarConfig<NoSettings> = rustak_core::config::load_str(text).unwrap();

        SidecarContext::from_config(config, "0.0.0-test", Shutdown::new()).unwrap()
    }

    #[test]
    fn a_connection_identifies_itself_as_the_service_the_file_names() {
        // The two things the server reads a subscription's identity out of: the
        // uid it keys on, and the callsign it shows. Both are derived, so an
        // operator never has a third place to keep them consistent.
        let context = context(
            r#"
            [service]
            name = "adsb"
            display_name = "ADS-B Heathrow"

            [server]
            stream = "tcp://tak.example.com:8089"
            "#,
        );

        let config = stream_config(&context, "tcp://tak.example.com:8089").unwrap();

        assert_eq!(config.uid, "SERVICE-adsb");
        assert_eq!(config.callsign.as_deref(), Some("ADS-B Heathrow"));
        assert_eq!(config.endpoint.port, 8089);
        assert!(config.tls.is_none());
        assert!(config.negotiate);
        assert!(!config.pass_control, "a plugin has no use for a pong");
    }

    #[test]
    fn a_tls_endpoint_without_certificate_material_is_refused_at_start_up() {
        // Refusing here names the setting; refusing at the handshake names a
        // TLS alert nobody can map back to a missing `[service] certificate`.
        let context = context(
            "[service]\nname = \"adsb\"\n\n[server]\nstream = \"ssl://tak.example.com:8089\"\n",
        );

        let Err(err) = Link::open(&context) else {
            panic!("a TLS endpoint needs certificate material");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("certificate"), "{err}");
    }

    #[test]
    fn an_unreadable_connect_string_is_refused_by_name() {
        let context = context(
            "[service]\nname = \"adsb\"\n\n[server]\nstream = \"amqp://tak.example.com:5672\"\n",
        );

        let Err(err) = Link::open(&context) else {
            panic!("'amqp' is not a transport a TAK stream speaks");
        };

        assert!(err.to_string().contains("amqp"), "{err}");
    }

    #[tokio::test]
    async fn a_sidecar_with_no_stream_configured_gets_a_link_that_never_fires() {
        // The property the whole `Option` exists for: one loop in `run`,
        // whether or not the plugin has a socket, and no busy-polling in the
        // sidecars that do not.
        let mut link = Link::open(&context("[service]\nname = \"adsb\"\n")).unwrap();

        assert!(link.endpoint().is_none());
        assert!(format!("{link:?}").contains("connected: false"));

        let polled = tokio::time::timeout(std::time::Duration::from_millis(50), link.next()).await;
        assert!(polled.is_err(), "an idle link should never be ready");

        // Publishing into one is a counted no-op rather than a failure.
        link.publish(vec![Event::builder("a-f-G", "SERVICE-adsb").build()])
            .await
            .unwrap();
    }

    /// The clock the discard tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    #[test]
    fn a_stream_that_has_never_connected_is_not_reconnecting() {
        // The production finding: every clean start logged `WARN Discarding
        // events: the CoT stream is reconnecting` because the first poll's
        // batch was published ~40ms before the first connect completed. It was
        // not reconnecting. It had never connected.
        let mut discards = Discards::default();

        assert_eq!(discards.discarded(17, at(0)), Discarded::Starting);
        assert_eq!(discards.discarded(17, at(1)), Discarded::Starting);
    }

    #[test]
    fn the_first_batch_lost_to_a_stream_that_dropped_is_the_one_that_is_announced() {
        let mut discards = Discards::default();

        discards.connected(true, at(0));
        discards.connected(false, at(10));

        assert_eq!(discards.discarded(4, at(11)), Discarded::First { count: 4 });
        assert_eq!(discards.discarded(4, at(12)), Discarded::Quiet);
        assert_eq!(discards.discarded(4, at(200)), Discarded::Quiet);
    }

    #[test]
    fn a_stream_that_is_still_down_says_how_much_has_been_lost_every_five_minutes() {
        let mut discards = Discards::default();

        discards.connected(true, at(0));
        discards.connected(false, at(1));
        discards.discarded(10, at(2));

        for second in [3, 100, 299] {
            assert_eq!(discards.discarded(10, at(second)), Discarded::Quiet);
        }

        assert_eq!(
            discards.discarded(10, at(302)),
            Discarded::Reminder {
                count: 50,
                failing_for: Duration::from_secs(300),
            },
            "the reminder carries everything thrown away since the last one",
        );
    }

    #[test]
    fn a_stream_that_came_back_announces_the_next_outage_again() {
        let mut discards = Discards::default();

        discards.connected(true, at(0));
        discards.connected(false, at(1));
        discards.discarded(3, at(2));
        discards.connected(true, at(5));
        discards.connected(false, at(9));

        assert_eq!(discards.discarded(3, at(10)), Discarded::First { count: 3 });
    }

    #[tokio::test]
    async fn settling_a_link_with_no_stream_answers_at_once() {
        // `--check`-shaped sidecars and every offline unit test: there is
        // nothing to wait for, and waiting ten seconds for it would be ten
        // seconds on every test that drives a streamless harness.
        let mut link = Link::open(&context("[service]\nname = \"adsb\"\n")).unwrap();

        let started = std::time::Instant::now();

        assert!(!link.settle(FIRST_CONNECT).await);
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[tokio::test]
    async fn settling_gives_up_on_a_server_that_is_not_there() {
        // Bounded, because a sidecar whose server is down still has to start:
        // past the wait the ordinary path takes over and the next tick
        // republishes.
        let context =
            context("[service]\nname = \"adsb\"\n\n[server]\nstream = \"tcp://127.0.0.1:1\"\n");
        let mut link = Link::open(&context).unwrap();

        assert!(!link.settle(std::time::Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn events_returned_while_the_connection_is_down_are_dropped_rather_than_queued() {
        // A position report delivered after a thirty-second backoff is worse
        // than one that was never sent, and a plugin that disagrees can return
        // it again from the next `Connected`.
        let context =
            context("[service]\nname = \"adsb\"\n\n[server]\nstream = \"tcp://127.0.0.1:1\"\n");
        let mut link = Link::open(&context).unwrap();

        link.publish(vec![Event::builder("a-f-G", "SERVICE-adsb").build()])
            .await
            .unwrap();
    }
}
