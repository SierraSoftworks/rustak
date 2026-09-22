//! [`CotTap`]: what is being relayed right now, for a reader that is not a TAK
//! client.
//!
//! The hub's subscribers are connections: each has a codec, a queue and a
//! place in the client listing. A browser drawing a map wants the same
//! messages and none of that, so the router also publishes what it relays
//! here, and whoever wants to watch subscribes.
//!
//! # The routing path never waits for a watcher
//!
//! A `broadcast` send is a refcount and a slot in a ring. A watcher that stops
//! reading lags and is told so; it cannot hold a message up, and it cannot
//! grow the ring. With nobody watching, publishing is one atomic read.
//!
//! # Who may be shown what is the watcher's question
//!
//! The tap carries the sender's channels beside each message rather than
//! deciding anything, because the answer depends on who is asking and that is
//! known only at the other end. It is the same pair `cot_latest` keeps, for
//! the same reason.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rustak_cot::codec::EncodedEvent;
use tokio::sync::broadcast;

use crate::prelude::*;

/// How many messages a watcher may fall behind by before it is told it lagged.
///
/// A few seconds of a busy feed. A watcher that is further behind than this
/// re-reads the snapshot, which is cheaper than replaying what it missed.
const RING: usize = 1024;

/// One relayed message, and who its sender was publishing to.
#[derive(Clone, Debug)]
pub struct RelayedCot {
    /// The message in its relayed form, shared with every recipient's writer.
    pub encoded: Arc<EncodedEvent>,

    /// The sender's channels at the time, which is the connection's own set.
    pub groups: Arc<GroupSet>,

    /// When this server handled it.
    pub received_at: DateTime<Utc>,
}

/// The publishing end, held by the router.
#[derive(Clone, Debug)]
pub struct CotTap {
    tx: broadcast::Sender<Arc<RelayedCot>>,
}

impl Default for CotTap {
    fn default() -> Self {
        Self::new()
    }
}

impl CotTap {
    /// A tap nobody is watching yet.
    pub fn new() -> Self {
        Self {
            tx: broadcast::channel(RING).0,
        }
    }

    /// Announces a relayed message to whoever is watching.
    pub fn publish(&self, encoded: &Arc<EncodedEvent>, sender: &Principal) {
        if self.tx.receiver_count() == 0 {
            return;
        }

        // An error means the last watcher left between the check and here.
        let _ = self.tx.send(Arc::new(RelayedCot {
            encoded: Arc::clone(encoded),
            groups: Arc::clone(&sender.groups),
            received_at: Utc::now(),
        }));
    }

    /// Starts watching. Only what is published from now on is received.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<RelayedCot>> {
        self.tx.subscribe()
    }

    /// How many watchers there are.
    pub fn watchers(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::Event;

    use super::*;

    fn sender() -> Principal {
        Principal::new(
            UserId::from(3),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
    }

    fn message(uid: &str) -> Arc<EncodedEvent> {
        Arc::new(EncodedEvent::new(
            Event::builder("a-f-G-U-C", uid).point(51.5, -0.12).build(),
        ))
    }

    #[tokio::test]
    async fn a_watcher_receives_what_is_published_after_it_subscribed() {
        let tap = CotTap::new();
        tap.publish(&message("BEFORE"), &sender());

        let mut watching = tap.subscribe();
        tap.publish(&message("AFTER"), &sender());

        assert_eq!(watching.recv().await.unwrap().encoded.event().uid, "AFTER");
        assert!(watching.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_watcher_that_stops_reading_lags_rather_than_holding_anything_up() {
        let tap = CotTap::new();
        let mut watching = tap.subscribe();

        for index in 0..=RING {
            tap.publish(&message(&format!("UID-{index}")), &sender());
        }

        assert!(matches!(
            watching.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
    }
}
