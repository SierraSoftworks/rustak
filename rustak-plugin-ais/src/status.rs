//! What the admin UI's Services page shows about this feed.
//!
//! A feed that is healthy and a feed whose upstream has been down for an hour
//! both publish nothing when there is nothing in the area, so "the process is
//! running" is not an answer an administrator can use. This module turns the
//! publisher's counters and the source's connection state into the
//! [`Heartbeat`] the control API takes, so that the Services page says
//! *connected, 412 vessels, 3 400 published* or *reconnecting since 12:04,
//! connection refused* without anybody reading a log.
//!
//! # How it reaches the server
//!
//! Through [`Sidecar::health`](rustak_client::sidecar::Sidecar::health), which
//! the harness asks after every tick and reports *instead of* its own
//! `Heartbeat::healthy()`. The server keeps the last heartbeat it was given, so
//! only one of the two can be the one an administrator sees; the hook is how a
//! plugin makes it this one. `AisSidecar::health` is three lines around
//! [`FeedStatus::heartbeat`].

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::FeedCounters;
use rustak_core::prelude::*;
use tokio::sync::watch;

/// How a source's connection to its upstream is doing.
///
/// Never carries a credential: the variants hold an instant and the text of an
/// error this crate wrote, and the sources are careful that neither is built
/// out of an API key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    /// Nothing has connected yet — the process has just started, or the
    /// upstream has refused every attempt since it did.
    #[default]
    Waiting,
    /// Talking to the upstream since this instant.
    Connected {
        /// When the current connection was established.
        since: DateTime<Utc>,
    },
    /// Trying to get back, since this instant.
    Reconnecting {
        /// When the connection was lost.
        since: DateTime<Utc>,
        /// Why, in the words this crate logged.
        last_error: String,
    },
}

impl Connection {
    /// A source that has no connection to lose — a file, or a bound socket —
    /// reports itself connected from the moment it opens.
    #[must_use]
    pub fn connected() -> Self {
        Self::Connected { since: Utc::now() }
    }

    /// The same, losing the connection now.
    #[must_use]
    pub fn reconnecting(reason: impl Into<String>) -> Self {
        Self::Reconnecting {
            since: Utc::now(),
            last_error: reason.into(),
        }
    }

    /// How long this state has held, or [`None`] for one that never started.
    #[must_use]
    pub fn held_for(&self, now: DateTime<Utc>) -> Option<Duration> {
        let since = match self {
            Self::Waiting => return None,
            Self::Connected { since } | Self::Reconnecting { since, .. } => *since,
        };

        (now - since).to_std().ok()
    }
}

/// Everything this sidecar reports about itself on a heartbeat.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeedStatus {
    /// The upstream's name, as the source calls itself.
    pub source: String,
    /// How that upstream is doing.
    pub connection: Connection,
    /// What the publisher has done.
    pub counters: FeedCounters,
    /// How many vessels it is holding.
    pub tracked: usize,
}

impl FeedStatus {
    /// The heartbeat this status posts.
    ///
    /// `poll` is the sidecar's tick interval: a source that has been
    /// reconnecting for longer than two of them has missed more than a blip,
    /// which is what makes a feed `degraded` rather than merely busy.
    #[must_use]
    pub fn heartbeat(&self, poll: Duration, now: DateTime<Utc>) -> Heartbeat {
        let (state, message) = self.state(poll, now);

        Heartbeat {
            state,
            message: Some(message),
            metrics: serde_json::json!({
                "offered": self.counters.offered,
                "published": self.counters.published,
                "suppressed": self.counters.suppressed,
                "expired": self.counters.expired,
                "tracked": self.tracked,
                "source": self.source_metric(),
            }),
        }
    }

    /// The `source` row of the metrics table: a flat object naming the upstream
    /// and how it is doing.
    ///
    /// One level of nesting, which is what `docs/plugins.md` → "Monitoring a
    /// sidecar" says the Services page renders as a heading with its fields
    /// indented under it.
    fn source_metric(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(&self.connection).unwrap_or_default();

        if let Some(fields) = value.as_object_mut() {
            fields.insert("kind".to_string(), self.source.clone().into());
        }

        value
    }

    /// The state and the sentence beside it.
    fn state(&self, poll: Duration, now: DateTime<Utc>) -> (ServiceState, String) {
        match &self.connection {
            Connection::Waiting => (
                ServiceState::Unhealthy,
                format!("{} has never answered.", self.source),
            ),
            Connection::Connected { .. } => (
                ServiceState::Healthy,
                format!(
                    "Connected to {}; {} vessels tracked, {} published.",
                    self.source, self.tracked, self.counters.published,
                ),
            ),
            Connection::Reconnecting { last_error, .. } => {
                let held = self.connection.held_for(now).unwrap_or_default();
                let state = match held > poll.saturating_mul(2) {
                    true => ServiceState::Degraded,
                    false => ServiceState::Healthy,
                };

                (
                    state,
                    format!(
                        "Reconnecting to {} after {}s: {last_error}",
                        self.source,
                        held.as_secs(),
                    ),
                )
            }
        }
    }
}

/// A handle a source writes its connection state through.
///
/// A [`watch`] channel rather than a lock: the sources write from their own
/// tasks, the plugin reads from its tick, and neither ever waits for the other.
pub type ConnectionTx = watch::Sender<Connection>;

/// The other end, which the plugin reads.
pub type ConnectionRx = watch::Receiver<Connection>;

/// Creates the pair, starting from "nothing has connected yet".
#[must_use]
pub fn connection() -> (ConnectionTx, ConnectionRx) {
    watch::channel(Connection::Waiting)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        "2026-09-20T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("an instant")
            + chrono::Duration::seconds(seconds)
    }

    fn status(connection: Connection) -> FeedStatus {
        FeedStatus {
            source: "aisstream.io".into(),
            connection,
            counters: FeedCounters {
                offered: 400,
                published: 120,
                suppressed: 280,
                expired: 4,
            },
            tracked: 96,
        }
    }

    #[test]
    fn a_source_that_has_never_connected_is_unhealthy() {
        let beat = status(Connection::Waiting).heartbeat(Duration::from_secs(5), at(0));

        assert_eq!(beat.state, ServiceState::Unhealthy);
        assert!(
            beat.message
                .as_deref()
                .is_some_and(|message| message.contains("never answered")),
            "{:?}",
            beat.message,
        );
    }

    #[test]
    fn a_connected_source_is_healthy_and_says_what_it_is_carrying() {
        let beat =
            status(Connection::Connected { since: at(0) }).heartbeat(Duration::from_secs(5), at(9));

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["tracked"], 96);
        assert_eq!(beat.metrics["published"], 120);
        assert_eq!(beat.metrics["suppressed"], 280);
        assert_eq!(beat.metrics["expired"], 4);
        assert_eq!(beat.metrics["source"]["kind"], "aisstream.io");
        assert_eq!(beat.metrics["source"]["state"], "connected");
        assert!(beat.metrics["source"]["since"].is_string());
    }

    #[test]
    fn a_blip_is_not_a_degradation_but_two_poll_intervals_of_one_is() {
        let dropped = Connection::Reconnecting {
            since: at(0),
            last_error: "connection reset".into(),
        };
        let poll = Duration::from_secs(5);

        assert_eq!(
            status(dropped.clone()).heartbeat(poll, at(9)).state,
            ServiceState::Healthy,
            "one reconnection inside two ticks is an ordinary Tuesday",
        );
        assert_eq!(
            status(dropped).heartbeat(poll, at(11)).state,
            ServiceState::Degraded,
        );
    }

    #[test]
    fn nothing_a_heartbeat_carries_could_be_a_credential() {
        // The metrics are rendered in the admin UI as-is, so this is the one
        // place a feed could leak the key it subscribes with.
        let beat = status(Connection::Reconnecting {
            since: at(0),
            last_error: "401 Unauthorized".into(),
        })
        .heartbeat(Duration::from_secs(5), at(30));

        let rendered = format!("{} {:?}", beat.metrics, beat.message);

        assert!(rendered.contains("401 Unauthorized"), "{rendered}");
        assert!(!rendered.contains("APIKey"), "{rendered}");
    }

    #[test]
    fn a_connection_reports_how_long_it_has_held() {
        assert_eq!(Connection::Waiting.held_for(at(30)), None);
        assert_eq!(
            Connection::Connected { since: at(0) }.held_for(at(30)),
            Some(Duration::from_secs(30)),
        );
        assert_eq!(
            Connection::reconnecting("nope").held_for(at(0)),
            None,
            "a state that starts in the future has held for no time at all",
        );
    }
}
