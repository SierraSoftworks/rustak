//! Relaying a message on behalf of somebody who has no connection.
//!
//! A marker placed in the admin console has a sender — the signed-in account
//! — but no socket, and everything routing knows about a sender it knows
//! through the hub: who it may reach, which channel it may publish into, whose
//! own report must not be sent back to it. Rather than teach the routing path
//! a second kind of sender, the sender is given a connection: an ephemeral
//! subscription registered for exactly one message, with nothing on the far
//! end of its queue and nobody told that it came or went. The message then
//! takes the path every other message takes — tag, record, tap, select, fan
//! out — and is byte for byte what a device would have produced.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use rustak_cot::Event;
use tokio::sync::mpsc;

use crate::prelude::*;

use super::router::{Disposition, Router};
use super::subscription::{ConnHandle, ConnStats, Subscription};

/// Where an ephemeral subscription says it connected from: nowhere.
const NOWHERE: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

impl Router {
    /// Relays `event` as though `sender` had sent it over a connection.
    ///
    /// Subject to everything a message from a device is subject to: a
    /// `<dest group>` the sender may not publish into is dropped, and a
    /// message that reaches nobody is still recorded and still reaches every
    /// open map — a marker on a server with no device connected is not
    /// nothing.
    pub async fn publish(&self, sender: Arc<Principal>, event: Event) -> Disposition {
        let id = self.hub().next_id();

        // Nothing reads the far end: a delivery *to* this subscription is a
        // delivery to nobody, and there is never one, because the sender's
        // own message is excluded from the implicit broadcast and no client
        // can address a connection with no callsign.
        let (tx, _unread) = mpsc::channel(1);
        let handle = ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 1, Shutdown::new());
        let fingerprint = format!("console:{}", sender.username);

        self.hub().register(
            Subscription::new(id, sender, Vec::new(), fingerprint, NOWHERE, handle).ephemeral(),
        );
        // Unregistered when this scope ends, however it ends: routing awaits
        // a channel lookup, and a request cancelled during it must not leave
        // a connection nobody will ever remove.
        let _standing_in = StandIn {
            hub: self.hub(),
            id,
        };

        self.handle_inbound(id, event).await
    }
}

/// An ephemeral subscription's presence in the hub, for as long as this is
/// held.
struct StandIn<'a> {
    hub: &'a super::Hub,
    id: super::subscription::ConnId,
}

impl Drop for StandIn<'_> {
    fn drop(&mut self) {
        self.hub.unregister(self.id);
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::detail::flow_tags;
    use rustak_cot::detail::marti::{Dest, marti_element};
    use rustak_cot::detail::{Contact, contact::STREAMING_ENDPOINT};

    use std::future::Future as _;

    use super::super::mission_hook;
    use super::super::subscription::Outbound;
    use super::super::{Hub, StreamMetrics};
    use super::*;
    use crate::cot_store::CotStoreHandle;
    use crate::db::Database;

    fn principal(name: &str, grants: &[(u32, Direction)]) -> Arc<Principal> {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        Arc::new(
            Principal::new(
                UserId::from(1),
                Username::parse(name).unwrap(),
                PrincipalKind::Person,
                AuthMethod::SetupToken,
            )
            .with_groups(Arc::new(groups)),
        )
    }

    /// A router with one device connected, publishing and receiving on bit 2.
    async fn router_with_a_device() -> (Router, mpsc::Receiver<Outbound>) {
        let router = Router::new(
            Arc::new(Hub::new()),
            Database::open_in_memory().await.unwrap(),
            CotStoreHandle::disabled(),
            mission_hook::no_missions(),
            Arc::new(StreamMetrics::default()),
            "rustak-test",
        );

        let id = router.hub().next_id();
        let (tx, rx) = mpsc::channel(16);
        router.hub().register(Subscription::new(
            id,
            principal("device", &[(2, Direction::Both)]),
            Vec::new(),
            "device".to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));
        router
            .handle_inbound(
                id,
                Event::builder("a-f-G-U-C", "DEVICE-1")
                    .point(51.5, -0.12)
                    .typed(&Contact::new("DEVICE").with_endpoint(STREAMING_ENDPOINT))
                    .build(),
            )
            .await;

        (router, rx)
    }

    fn marker() -> Event {
        Event::builder("a-u-G", "MARKER-1")
            .how("h-g-i-g-o")
            .point(51.51, -0.11)
            .typed(&Contact::new("MARKER 1"))
            .build()
    }

    #[tokio::test]
    async fn a_published_message_reaches_the_devices_its_sender_reaches_and_is_tagged() {
        let (router, mut rx) = router_with_a_device().await;

        let relayed = router
            .publish(principal("grace", &[(2, Direction::In)]), marker())
            .await;

        assert!(
            matches!(relayed, Disposition::Relayed { recipients: 1, .. }),
            "{relayed:?}"
        );
        let Some(Outbound::Event(received)) = rx.try_recv().ok() else {
            panic!("the device was sent the marker");
        };
        assert_eq!(received.event().uid, "MARKER-1");
        assert!(flow_tags::has_flow_tag(
            &received.event().detail,
            "rustak-test"
        ));

        assert_eq!(router.hub().len(), 1, "the stand-in connection is gone");
    }

    #[tokio::test]
    async fn a_publish_dropped_mid_way_leaves_nothing_behind_in_the_hub() {
        let (router, _rx) = router_with_a_device().await;

        // Polled once — far enough to register — and then dropped, as a
        // cancelled request would be.
        let mut publishing =
            Box::pin(router.publish(principal("grace", &[(2, Direction::In)]), marker()));
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let _ = publishing.as_mut().poll(&mut context);
        drop(publishing);

        assert_eq!(router.hub().len(), 1, "only the device is left");
    }

    #[tokio::test]
    async fn a_channel_the_sender_may_not_publish_into_is_refused_as_it_is_for_a_device() {
        let (router, mut rx) = router_with_a_device().await;

        let mut event = marker();
        event.detail.push(marti_element(&[Dest::group("nowhere")]));

        let dropped = router
            .publish(principal("grace", &[(2, Direction::In)]), event)
            .await;

        assert!(matches!(dropped, Disposition::Dropped(_)), "{dropped:?}");
        assert!(rx.try_recv().is_err(), "nothing reached the device");
    }

    #[tokio::test]
    async fn a_sender_who_reaches_nobody_still_publishes_to_the_tap() {
        let (router, mut rx) = router_with_a_device().await;
        let mut seen = router.tap().subscribe();

        let dropped = router.publish(principal("grace", &[]), marker()).await;

        assert!(matches!(dropped, Disposition::Dropped(_)), "{dropped:?}");
        assert!(rx.try_recv().is_err());
        assert_eq!(seen.try_recv().unwrap().encoded.event().uid, "MARKER-1");
    }
}
