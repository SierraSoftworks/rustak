//! Filling a newly connected client's map.
//!
//! A TAK client that has just connected knows about nobody. Waiting for the
//! next position report from each peer would leave the map empty for up to a
//! reporting interval — tens of seconds on a fleet that reports sparingly — so
//! the server pushes the last situational-awareness message it holds for every
//! peer the newcomer is allowed to see (`compat/streaming.md` §4 step 2).
//!
//! # This happens before the protocol offer
//!
//! The replay is plain XML, because the connection has not been offered
//! protobuf yet and every client understands XML. Both go through the same
//! per-connection writer queue, so the ordering is guaranteed by the queue
//! rather than by a wait: replay first, then the single `t-x-takp-v`.

use std::time::Duration;

use super::hub::Hub;
use super::metrics::StreamMetrics;
use super::subscription::{ConnId, Outbound, SendResult};
use crate::prelude::*;

/// Pushes the latest position of every peer `to` may see.
///
/// Returns how many were queued.
///
/// # Why this waits
///
/// The replay is as many messages as there are peers, pushed into a queue that
/// is `[stream.limits] queue_len` deep, before the client has read a byte. On a
/// fleet larger than the queue the two do not fit, and the version of this that
/// pushed with `try_send` both discarded the overflow *and* counted it as
/// consecutive drops — so past `close_after_drops` the connection was closed
/// during its own replay and the fleet sat in a reconnect loop (R-03 C1).
///
/// So it sends through [`ConnHandle::send_replay`], which awaits room and does
/// not touch the drop counter. The writer task is already running, so the wait
/// is real backpressure on one joining client rather than a stall: the cost of
/// a queue shallower than the fleet is paid in the joining client's own
/// connect time, and `config/validate` warns an installation that would pay it.
///
/// [`ConnHandle::send_replay`]: super::subscription::ConnHandle::send_replay
///
/// # Why there is a deadline
///
/// A peer that completed the handshake and then stopped reading would otherwise
/// hold this loop for ever, one full queue at a time, on a task that has not
/// started reading and so cannot be reclaimed by the idle timeout. `within`
/// bounds the whole replay, not each message: an incomplete map is a map that
/// fills on the next reporting interval, and the connection is still worth
/// serving.
pub async fn replay_latest_sa(
    hub: &Hub,
    to: ConnId,
    within: Duration,
    metrics: &StreamMetrics,
) -> usize {
    let Some(handle) = hub.handle(to) else {
        return 0;
    };

    let events = hub.latest_sa_for(to);
    let wanted = events.len();
    let deadline = tokio::time::Instant::now() + within;
    let mut sent = 0;

    for event in events {
        if handle.is_closing() {
            break;
        }

        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if handle.send_replay(Outbound::Event(event), left).await != SendResult::Sent {
            break;
        }

        sent += 1;
    }

    match wanted - sent {
        0 if sent > 0 => {
            debug!(conn = %to, peers = sent, "Replayed the latest position of the peers a new client can see.");
        }
        0 => {}
        missing => {
            StreamMetrics::add(&metrics.replay_dropped, missing as u64);
            warn!(
                conn = %to,
                peers = wanted,
                sent,
                missing,
                "A new client's map was not fully replayed within its budget; it will fill as its peers report.",
            );
        }
    }

    sent
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rustak_cot::Event;
    use rustak_cot::codec::EncodedEvent;
    use rustak_cot::detail::{Contact, Group, contact::STREAMING_ENDPOINT};
    use tokio::sync::mpsc;

    use super::super::subscription::{ConnHandle, ConnStats, Subscription};
    use super::*;

    /// The budget every test here replays within. Long enough that a healthy
    /// queue never reaches it, short enough that a test that hangs fails.
    const BUDGET: Duration = Duration::from_secs(5);

    fn join(
        hub: &Hub,
        name: &str,
        grants: &[(u32, Direction)],
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        join_with_queue(hub, name, grants, 8)
    }

    fn join_with_queue(
        hub: &Hub,
        name: &str,
        grants: &[(u32, Direction)],
        queue_len: usize,
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        let id = hub.next_id();
        let (tx, rx) = mpsc::channel(queue_len);

        hub.register(Subscription::new(
            id,
            Arc::new(
                Principal::new(
                    UserId::from(1),
                    Username::parse(name).unwrap(),
                    PrincipalKind::Person,
                    AuthMethod::SetupToken,
                )
                .with_groups(Arc::new(groups)),
            ),
            Vec::new(),
            format!("{name:f>64}"),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));

        (id, rx)
    }

    fn identify(hub: &Hub, id: ConnId, uid: &str, callsign: &str) {
        let event = Event::builder("a-f-G-U-C", uid)
            .point(51.5, -0.12)
            .stale_after(Duration::from_secs(60))
            .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
            .typed(&Group::new("Cyan", "Team Member"))
            .build();
        let encoded = Arc::new(EncodedEvent::new(event.clone()));

        hub.apply_event(id, &event, Some(&encoded));
    }

    #[tokio::test]
    async fn a_new_client_is_given_the_peers_it_may_see() {
        let hub = Hub::new();
        let metrics = StreamMetrics::default();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (stranger, _stranger_rx) = join(&hub, "stranger", &[(9, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");
        identify(&hub, stranger, "UID-S", "STRANGE");

        let (newcomer, mut newcomer_rx) = join(&hub, "newcomer", &[(7, Direction::Both)]);

        assert_eq!(replay_latest_sa(&hub, newcomer, BUDGET, &metrics).await, 1);

        let Ok(Outbound::Event(event)) = newcomer_rx.try_recv() else {
            panic!("the replayed position should be queued");
        };
        assert_eq!(event.event().uid, "UID-A");
        assert!(newcomer_rx.try_recv().is_err(), "and nothing else");
    }

    #[tokio::test]
    async fn a_client_alone_on_the_server_is_sent_nothing() {
        let hub = Hub::new();
        let metrics = StreamMetrics::default();
        let (only, mut only_rx) = join(&hub, "only", &[(7, Direction::Both)]);

        assert_eq!(replay_latest_sa(&hub, only, BUDGET, &metrics).await, 0);
        assert!(only_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_client_joining_a_full_house_receives_all_of_it_and_is_not_closed() {
        // R-03 C1. 900 peers, a 256-deep queue — the shipped default — and a
        // writer that drains as the replay pushes. The version that pushed with
        // `try_send` queued the first 256, counted the next 512 as consecutive
        // drops, and closed the connection on the 513th: a fleet above ~770
        // devices could not reconnect after a restart.
        let hub = Hub::new();
        let metrics = StreamMetrics::default();

        // The receivers are kept alive for the length of the test so that every
        // peer looks like a live connection rather than a closed one.
        let mut peers = Vec::new();
        for peer in 0..900 {
            let name = format!("peer{peer}");
            let (id, rx) = join(&hub, &name, &[(7, Direction::Both)]);
            identify(&hub, id, &format!("UID-{peer}"), &format!("CS-{peer}"));
            peers.push(rx);
        }

        let (newcomer, mut newcomer_rx) =
            join_with_queue(&hub, "newcomer", &[(7, Direction::Both)], 256);
        let handle = hub.handle(newcomer).expect("the newcomer is registered");

        // Stands in for the writer task, which is running by the time the
        // connection replays.
        let reader = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(Outbound::Event(event)) = newcomer_rx.recv().await {
                seen.push(event.event().uid.clone());
            }
            seen
        });

        let sent = replay_latest_sa(&hub, newcomer, BUDGET, &metrics).await;
        drop(handle);
        hub.unregister(newcomer);

        assert_eq!(sent, 900, "every reachable peer is replayed");
        assert_eq!(StreamMetrics::get(&metrics.replay_dropped), 0);

        let seen = reader.await.expect("the reader finishes");
        assert_eq!(seen.len(), 900, "and every one of them arrives");
    }

    #[tokio::test]
    async fn a_replay_nobody_is_reading_gives_up_rather_than_waiting_for_ever() {
        // The other half of the same change: awaiting room must not become a
        // way for one peer that completed its handshake and then stopped
        // reading to park a connection task indefinitely.
        let hub = Hub::new();
        let metrics = StreamMetrics::default();

        let mut peers = Vec::new();
        for peer in 0..20 {
            let name = format!("peer{peer}");
            let (id, rx) = join(&hub, &name, &[(7, Direction::Both)]);
            identify(&hub, id, &format!("UID-{peer}"), &format!("CS-{peer}"));
            peers.push(rx);
        }

        // A queue of four and nothing draining it.
        let (newcomer, _newcomer_rx) =
            join_with_queue(&hub, "newcomer", &[(7, Direction::Both)], 4);

        let sent = replay_latest_sa(&hub, newcomer, Duration::from_millis(50), &metrics).await;

        assert_eq!(sent, 4, "the queue took what it could hold");
        assert_eq!(StreamMetrics::get(&metrics.replay_dropped), 16);
        assert!(
            !hub.handle(newcomer).expect("still registered").is_closing(),
            "a replay that did not fit is not a reason to close the connection",
        );
    }
}
