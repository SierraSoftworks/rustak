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

use super::hub::Hub;
use super::subscription::{ConnId, Outbound, SendResult};
use crate::prelude::*;

/// Pushes the latest position of every peer `to` may see.
///
/// Returns how many were queued. A connection whose queue is already full at
/// this point is one whose writer has not started yet, which cannot happen —
/// but if it somehow did, dropping a replay is better than refusing the
/// connection over it.
pub fn replay_latest_sa(hub: &Hub, to: ConnId) -> usize {
    let Some(handle) = hub.handle(to) else {
        return 0;
    };

    let events = hub.latest_sa_for(to);
    let mut sent = 0;

    for event in events {
        if handle.send(Outbound::Event(event)) == SendResult::Sent {
            sent += 1;
        }
    }

    if sent > 0 {
        debug!(conn = %to, peers = sent, "Replayed the latest position of the peers a new client can see.");
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

    fn join(
        hub: &Hub,
        name: &str,
        grants: &[(u32, Direction)],
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        let id = hub.next_id();
        let (tx, rx) = mpsc::channel(8);

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

    #[test]
    fn a_new_client_is_given_the_peers_it_may_see() {
        let hub = Hub::new();
        let (alpha, _alpha_rx) = join(&hub, "alpha", &[(7, Direction::Both)]);
        let (stranger, _stranger_rx) = join(&hub, "stranger", &[(9, Direction::Both)]);
        identify(&hub, alpha, "UID-A", "ALPHA");
        identify(&hub, stranger, "UID-S", "STRANGE");

        let (newcomer, mut newcomer_rx) = join(&hub, "newcomer", &[(7, Direction::Both)]);

        assert_eq!(replay_latest_sa(&hub, newcomer), 1);

        let Ok(Outbound::Event(event)) = newcomer_rx.try_recv() else {
            panic!("the replayed position should be queued");
        };
        assert_eq!(event.event().uid, "UID-A");
        assert!(newcomer_rx.try_recv().is_err(), "and nothing else");
    }

    #[test]
    fn a_client_alone_on_the_server_is_sent_nothing() {
        let hub = Hub::new();
        let (only, mut only_rx) = join(&hub, "only", &[(7, Direction::Both)]);

        assert_eq!(replay_latest_sa(&hub, only), 0);
        assert!(only_rx.try_recv().is_err());
    }
}
