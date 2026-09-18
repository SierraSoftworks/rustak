//! What happens to a message between arriving and being delivered.
//!
//! Order matters, and it is the order `compat/streaming.md` fixes:
//!
//! 1. A control message is consumed here and relayed to nobody.
//! 2. A message already carrying this server's flow tag has been here before
//!    and is dropped, which is what stops two bridged servers from looping.
//! 3. `<marti>` is taken off — **every** `<marti>`, including one addressed to
//!    nobody — so that recipients never learn the address list.
//! 4. An incognito sender that named nobody explicitly is dropped.
//! 5. This server's flow tag is added, and only then is the message encoded.
//! 6. The encoded message updates the sender's subscription (which is how the
//!    replay copy comes to be the *relayed* form) and is handed to the store.
//! 7. Recipients are selected and the same `Arc` is handed to each of them.
//!
//! # Routing runs on the sender's task
//!
//! There is no router task. `handle_inbound` is called from the connection that
//! read the message, takes the hub lock for as long as a hash-map scan, and
//! hands out `Arc` clones. The only `await`s on this path are the mission hook
//! and the channel lookup a `<dest group>` needs, and neither happens for
//! ordinary traffic.

use std::sync::Arc;

use rustak_cot::codec::EncodedEvent;
use rustak_cot::detail::flow_tags;
use rustak_cot::detail::marti::take_marti;
use rustak_cot::{CotTime, Event};

use crate::cot_store::{CotRecord, CotStoreHandle};
use crate::db::Database;
use crate::prelude::*;

use super::control::{self, ControlAction};
use super::dest::{self, DropReason};
use super::hub::Hub;
use super::metrics::StreamMetrics;
use super::mission_hook::MissionIngest;
use super::subscription::{ConnId, Outbound, SendResult};

/// What became of one inbound message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// It was a control message and was answered, or deliberately not.
    Control(ControlAction),
    /// It reached nobody, for a reason.
    Dropped(DropReason),
    /// It was relayed.
    Relayed {
        /// How many connections it was queued for.
        recipients: usize,
        /// Whether the sender named them.
        explicit: bool,
    },
}

/// Everything routing one message needs.
#[derive(Clone)]
pub struct Router {
    hub: Arc<Hub>,
    db: Database,
    store: CotStoreHandle,
    missions: Arc<dyn MissionIngest>,
    metrics: Arc<StreamMetrics>,
    server_id: String,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("server_id", &self.server_id)
            .field("connections", &self.hub.len())
            .finish_non_exhaustive()
    }
}

impl Router {
    /// Builds a router over a hub.
    pub fn new(
        hub: Arc<Hub>,
        db: Database,
        store: CotStoreHandle,
        missions: Arc<dyn MissionIngest>,
        metrics: Arc<StreamMetrics>,
        server_id: impl Into<String>,
    ) -> Self {
        Self {
            hub,
            db,
            store,
            missions,
            metrics,
            server_id: server_id.into(),
        }
    }

    /// The registry this router delivers through.
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The counters this router keeps.
    pub fn metrics(&self) -> &Arc<StreamMetrics> {
        &self.metrics
    }

    /// The identifier this server stamps its flow tags with.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Handles one message from one connection.
    pub async fn handle_inbound(&self, from: ConnId, mut event: Event) -> Disposition {
        let now = CotTime::now();

        if event.is_control() {
            return Disposition::Control(control::handle(&self.hub, from, &event, now));
        }

        if flow_tags::has_flow_tag(&event.detail, &self.server_id) {
            StreamMetrics::incr(&self.metrics.dropped_flowtag);

            return Disposition::Dropped(DropReason::DuplicateFlowTag);
        }

        let dests = take_marti(&mut event.detail);

        let Some(sender) = self.hub.principal(from) else {
            // The connection went away between the read and here.
            return Disposition::Dropped(DropReason::NoRecipients);
        };

        if self.hub.is_incognito(from) && !dests.iter().any(|dest| dest.callsign.is_some()) {
            StreamMetrics::incr(&self.metrics.dropped_incognito);

            return Disposition::Dropped(DropReason::Incognito);
        }

        flow_tags::add_flow_tag(&mut event.detail, &self.server_id, now);

        let encoded = Arc::new(EncodedEvent::new(event));

        // After tagging, so the replay copy a newcomer receives is byte for byte
        // what this message's own recipients were sent.
        self.hub.apply_event(from, encoded.event(), Some(&encoded));
        self.record(from, &sender, &encoded);

        let selection = match dest::select_recipients(
            &self.hub,
            &self.db,
            self.missions.as_ref(),
            from,
            &sender,
            &dests,
            &encoded,
        )
        .await
        {
            Ok(selection) => selection,
            Err(reason) => {
                debug!(
                    uid = %encoded.event().uid,
                    reason = reason.as_str(),
                    "A message reached nobody."
                );

                return Disposition::Dropped(reason);
            }
        };

        if selection.handles.is_empty() {
            StreamMetrics::incr(&self.metrics.no_recipients);

            return Disposition::Dropped(DropReason::NoRecipients);
        }

        let recipients = self.fan_out(&selection.handles, &encoded);

        Disposition::Relayed {
            recipients,
            explicit: selection.explicit,
        }
    }

    /// Hands the same encoded message to every recipient.
    fn fan_out(
        &self,
        handles: &[super::subscription::ConnHandle],
        encoded: &Arc<EncodedEvent>,
    ) -> usize {
        let mut delivered = 0;

        for handle in handles {
            match handle.send(Outbound::Event(Arc::clone(encoded))) {
                SendResult::Sent => delivered += 1,
                SendResult::Dropped => StreamMetrics::incr(&self.metrics.dropped_queue),
                SendResult::Closed => {
                    StreamMetrics::incr(&self.metrics.dropped_queue);
                    StreamMetrics::incr(&self.metrics.closed_slow);
                }
            }
        }

        StreamMetrics::add(&self.metrics.tx_msgs, delivered as u64);

        delivered
    }

    /// Queues the message for storage, if the installation records anything.
    fn record(&self, from: ConnId, sender: &Principal, encoded: &Arc<EncodedEvent>) {
        if !self.store.is_enabled() {
            return;
        }

        self.store
            .record(CotRecord::new(encoded, sender, self.hub.device_id(from)));
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::detail::marti::{ALL_STREAMING, Dest, marti_element};
    use rustak_cot::detail::{Contact, Element, contact::STREAMING_ENDPOINT, flow_tags};
    use rustak_cot::types::cot_type;
    use tokio::sync::mpsc;

    use super::super::mission_hook;
    use super::super::subscription::{ConnHandle, ConnStats, Subscription};
    use super::*;

    /// This server's own identifier in the tests below.
    const SERVER_ID: &str = "rustak-test";

    /// A router over an empty hub and a database nothing is recorded to.
    async fn router() -> Router {
        Router::new(
            Arc::new(Hub::new()),
            Database::open_in_memory().await.unwrap(),
            CotStoreHandle::disabled(),
            mission_hook::no_missions(),
            Arc::new(StreamMetrics::default()),
            SERVER_ID,
        )
    }

    fn join(
        router: &Router,
        name: &str,
        grants: &[(u32, Direction)],
    ) -> (ConnId, mpsc::Receiver<Outbound>) {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        let id = router.hub().next_id();
        let (tx, rx) = mpsc::channel(16);

        router.hub().register(Subscription::new(
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

    fn sa(uid: &str, callsign: &str) -> Event {
        Event::builder("a-f-G-U-C", uid)
            .how("m-g")
            .point(51.5, -0.12)
            .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
            .build()
    }

    fn received(rx: &mut mpsc::Receiver<Outbound>) -> Option<Event> {
        match rx.try_recv().ok()? {
            Outbound::Event(event) => Some(event.event().clone()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_broadcast_reaches_the_reachable_and_not_the_sender() {
        let router = router().await;
        let (alpha, mut alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);
        let (_stranger, mut stranger_rx) = join(&router, "stranger", &[(9, Direction::Both)]);

        let disposition = router.handle_inbound(alpha, sa("UID-A", "ALPHA")).await;

        assert_eq!(
            disposition,
            Disposition::Relayed {
                recipients: 1,
                explicit: false
            }
        );
        assert!(received(&mut bravo_rx).is_some());
        assert!(received(&mut alpha_rx).is_none());
        assert!(received(&mut stranger_rx).is_none());
    }

    #[tokio::test]
    async fn a_relayed_message_is_stamped_and_has_its_address_list_removed() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        let mut event = sa("UID-A", "ALPHA");
        event
            .detail
            .push(marti_element(&[Dest::callsign("NOBODY")]));

        router.handle_inbound(alpha, event).await;

        let relayed = received(&mut bravo_rx);
        // Nobody is called NOBODY, so the callsign list selected nobody and the
        // message was not relayed at all — which is the point of the next
        // assertion pair.
        assert!(relayed.is_none());

        let mut broadcast = sa("UID-A", "ALPHA");
        broadcast.detail.push(marti_element(&[]));
        router.handle_inbound(alpha, broadcast).await;

        let seen = received(&mut bravo_rx).expect("an empty <marti> is still a broadcast");
        assert!(
            seen.detail.find("marti").is_none(),
            "<marti> never survives"
        );
        assert!(flow_tags::has_flow_tag(&seen.detail, SERVER_ID));
    }

    #[tokio::test]
    async fn a_message_that_has_been_here_before_is_dropped() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        let mut event = sa("UID-A", "ALPHA");
        flow_tags::add_flow_tag(&mut event.detail, SERVER_ID, CotTime::now());

        assert_eq!(
            router.handle_inbound(alpha, event).await,
            Disposition::Dropped(DropReason::DuplicateFlowTag)
        );
        assert!(received(&mut bravo_rx).is_none());
    }

    #[tokio::test]
    async fn another_servers_flow_tag_is_carried_rather_than_treated_as_our_own() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        let mut event = sa("UID-A", "ALPHA");
        event.detail.push(Element::new(flow_tags::ELEMENT).attr(
            flow_tags::flow_tag_name("somebody-else"),
            "2026-09-18T00:00:00.000Z",
        ));

        router.handle_inbound(alpha, event).await;

        let seen = received(&mut bravo_rx).expect("a message from another server is still relayed");
        assert!(flow_tags::has_flow_tag(&seen.detail, "somebody-else"));
        assert!(flow_tags::has_flow_tag(&seen.detail, SERVER_ID));
    }

    #[tokio::test]
    async fn a_control_message_is_consumed_and_never_relayed() {
        let router = router().await;
        let (alpha, mut alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        let disposition = router
            .handle_inbound(alpha, rustak_cot::msgs::ping("UID-A", CotTime::now()))
            .await;

        assert!(matches!(disposition, Disposition::Control(_)));
        assert_eq!(
            received(&mut alpha_rx).map(|event| event.r#type),
            Some(cot_type::PONG.to_string()),
        );
        assert!(received(&mut bravo_rx).is_none());
    }

    #[tokio::test]
    async fn an_incognito_sender_reaches_only_who_it_names() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        router.handle_inbound(bravo, sa("UID-B", "BRAVO")).await;
        let _ = received(&mut bravo_rx);
        router.hub().set_incognito(alpha, true);

        assert_eq!(
            router.handle_inbound(alpha, sa("UID-A", "ALPHA")).await,
            Disposition::Dropped(DropReason::Incognito)
        );

        let mut addressed = sa("UID-A", "ALPHA");
        addressed.uid = "UID-CHAT".into();
        addressed
            .detail
            .push(marti_element(&[Dest::callsign("BRAVO")]));

        assert_eq!(
            router.handle_inbound(alpha, addressed).await,
            Disposition::Relayed {
                recipients: 1,
                explicit: true
            }
        );
        assert!(received(&mut bravo_rx).is_some());
    }

    #[tokio::test]
    async fn all_streaming_degrades_an_address_list_to_a_broadcast() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);
        let (echo, mut echo_rx) = join(&router, "echo", &[(7, Direction::Both)]);

        router.handle_inbound(bravo, sa("UID-B", "BRAVO")).await;
        router.handle_inbound(echo, sa("UID-E", "ECHO")).await;
        let _ = received(&mut bravo_rx);
        let _ = received(&mut echo_rx);

        let mut event = sa("UID-A", "ALPHA");
        event.detail.push(marti_element(&[
            Dest::callsign("BRAVO"),
            Dest::callsign(ALL_STREAMING),
        ]));

        assert_eq!(
            router.handle_inbound(alpha, event).await,
            Disposition::Relayed {
                recipients: 2,
                explicit: false
            }
        );
        assert!(received(&mut bravo_rx).is_some());
        assert!(received(&mut echo_rx).is_some());
    }

    #[tokio::test]
    async fn a_dest_publish_addresses_nobody_and_does_not_fall_back_to_a_broadcast() {
        // TAK Server never implemented publish topics either; a message
        // carrying one must not become a broadcast by accident.
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        let mut event = sa("UID-A", "ALPHA");
        event.detail.push(marti_element(&[Dest {
            publish: Some("topic".into()),
            ..Dest::default()
        }]));

        assert_eq!(
            router.handle_inbound(alpha, event).await,
            Disposition::Dropped(DropReason::NoRecipients)
        );
        assert!(received(&mut bravo_rx).is_none());
    }

    #[tokio::test]
    async fn a_channel_dest_naming_a_channel_that_does_not_exist_reaches_nobody() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        let (_bravo, mut bravo_rx) = join(&router, "bravo", &[(7, Direction::Both)]);

        let mut event = sa("UID-A", "ALPHA");
        event.detail.push(marti_element(&[Dest::group("nowhere")]));

        assert_eq!(
            router.handle_inbound(alpha, event).await,
            Disposition::Dropped(DropReason::NoSuchGroup("nowhere".into()))
        );
        assert!(received(&mut bravo_rx).is_none());
    }

    #[tokio::test]
    async fn the_cached_replay_copy_is_the_relayed_form() {
        // What a newcomer is replayed has to be byte for byte what this
        // message's own recipients were sent — tag on, address list off.
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);

        let mut event = sa("UID-A", "ALPHA");
        event.detail.push(marti_element(&[]));
        router.handle_inbound(alpha, event).await;

        let (newcomer, _newcomer_rx) = join(&router, "newcomer", &[(7, Direction::Both)]);
        let replay = router.hub().latest_sa_for(newcomer);

        assert_eq!(replay.len(), 1);
        let cached = replay[0].event();
        assert!(cached.detail.find("marti").is_none());
        assert!(flow_tags::has_flow_tag(&cached.detail, SERVER_ID));
    }

    #[tokio::test]
    async fn a_message_from_a_connection_that_has_gone_reaches_nobody() {
        let router = router().await;
        let (alpha, _alpha_rx) = join(&router, "alpha", &[(7, Direction::Both)]);
        router.hub().unregister(alpha);

        assert_eq!(
            router.handle_inbound(alpha, sa("UID-A", "ALPHA")).await,
            Disposition::Dropped(DropReason::NoRecipients)
        );
    }
}
