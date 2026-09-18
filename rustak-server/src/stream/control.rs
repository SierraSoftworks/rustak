//! The message types that are consumed here and never relayed.
//!
//! `compat/streaming.md` §7 lists them: the keepalive and its reply, the
//! protocol request, the incognito toggles, the metrics report and the
//! client-side filter update. Two rules run through all of them.
//!
//! **A control message is answered privately or not at all.** The pong goes
//! back to the connection that pinged, with no flow tag and no channel check —
//! it is a fact about the socket, not about the network.
//!
//! **An unrecognised control type is a no-op, never an error.** TAK clients
//! send types we have never heard of, and a server that closed a connection
//! over one would be a server that drops working clients on their next update.
//! The connection is never disturbed by something on it that we do not
//! understand.

use std::sync::Arc;

use rustak_cot::codec::EncodedEvent;
use rustak_cot::types::cot_type;
use rustak_cot::{CotTime, Event, msgs};

use crate::prelude::*;

use super::hub::Hub;
use super::subscription::{ConnId, Outbound, SendResult};

/// What handling a control message did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAction {
    /// A keepalive was answered.
    Pong,
    /// The sender entered (`true`) or left (`false`) incognito.
    Incognito(bool),
    /// Recognised and deliberately not acted on, for the reason named.
    Ignored(&'static str),
}

/// Handles a control message, and says what it did.
///
/// `now` is passed in rather than read here so that the pong a test asserts on
/// is the one the test chose the instant for.
pub fn handle(hub: &Hub, from: ConnId, event: &Event, now: CotTime) -> ControlAction {
    if msgs::is_ping(event) {
        return pong(hub, from, now);
    }

    if let Some(incognito) = msgs::incognito_toggle(event) {
        hub.set_incognito(from, incognito);

        debug!(conn = %from, incognito, "A client changed its visibility.");

        return ControlAction::Incognito(incognito);
    }

    match event.r#type.as_str() {
        cot_type::PONG => ControlAction::Ignored("a stray keepalive reply"),
        cot_type::METRICS => ControlAction::Ignored("a client metrics report"),
        cot_type::FILTER => ControlAction::Ignored("a client-side geospatial filter"),
        cot_type::TAKP_Q => ControlAction::Ignored("a protocol request outside negotiation"),
        _ => ControlAction::Ignored("an unrecognised control type"),
    }
}

/// Answers a keepalive on the connection that sent it.
///
/// Built and encoded per ping rather than cached, because the template carries
/// the current time and a client that is checking staleness would otherwise see
/// a reply older than its own request.
fn pong(hub: &Hub, to: ConnId, now: CotTime) -> ControlAction {
    let Some(handle) = hub.handle(to) else {
        return ControlAction::Ignored("a keepalive from a connection that has gone");
    };

    let reply = Arc::new(EncodedEvent::new(msgs::pong(now)));

    if handle.send(Outbound::Event(reply)) != SendResult::Sent {
        debug!(conn = %to, "Could not answer a keepalive; the connection is behind.");
    }

    ControlAction::Pong
}

#[cfg(test)]
mod tests {
    use rustak_cot::detail::Element;
    use tokio::sync::mpsc;

    use super::super::subscription::{ConnHandle, ConnStats, Subscription};
    use super::*;

    fn hub_with_one() -> (Hub, ConnId, mpsc::Receiver<Outbound>) {
        let hub = Hub::new();
        let id = hub.next_id();
        let (tx, rx) = mpsc::channel(8);

        hub.register(Subscription::new(
            id,
            Arc::new(Principal::new(
                UserId::from(1),
                Username::parse("alice").unwrap(),
                PrincipalKind::Person,
                AuthMethod::SetupToken,
            )),
            Vec::new(),
            "ab".repeat(32),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));

        (hub, id, rx)
    }

    fn sent(rx: &mut mpsc::Receiver<Outbound>) -> Option<Event> {
        match rx.try_recv().ok()? {
            Outbound::Event(event) => Some(event.event().clone()),
            _ => None,
        }
    }

    #[test]
    fn a_ping_is_answered_on_the_connection_that_sent_it() {
        let (hub, id, mut rx) = hub_with_one();
        let now = CotTime::from_millis(1_789_646_400_000);

        assert_eq!(
            handle(&hub, id, &msgs::ping("UID-A", now), now),
            ControlAction::Pong
        );

        let reply = sent(&mut rx).expect("a pong");
        assert_eq!(reply.r#type, cot_type::PONG);
        assert_eq!(
            reply.uid,
            msgs::PONG_UID,
            "the uid is the constant, not derived"
        );
        assert!(
            reply.detail.is_empty(),
            "the pong carries no <detail> at all"
        );
    }

    #[test]
    fn the_incognito_toggles_change_only_the_sender() {
        let (hub, id, _rx) = hub_with_one();
        let now = CotTime::now();

        let on = Event::builder(cot_type::INCOGNITO_ON, "UID-A")
            .point(0.0, 0.0)
            .build();
        assert_eq!(handle(&hub, id, &on, now), ControlAction::Incognito(true));
        assert!(hub.is_incognito(id));

        let off = Event::builder(cot_type::INCOGNITO_OFF, "UID-A")
            .point(0.0, 0.0)
            .build();
        assert_eq!(handle(&hub, id, &off, now), ControlAction::Incognito(false));
        assert!(!hub.is_incognito(id));
    }

    #[test]
    fn a_stray_pong_is_ignored_rather_than_answered() {
        // Some clients echo one back; answering it would start a ping-pong loop
        // that never ends.
        let (hub, id, mut rx) = hub_with_one();

        assert!(matches!(
            handle(&hub, id, &msgs::pong(CotTime::now()), CotTime::now()),
            ControlAction::Ignored(_)
        ));
        assert!(sent(&mut rx).is_none());
    }

    #[test]
    fn a_control_type_we_do_not_know_never_disturbs_the_connection() {
        let (hub, id, mut rx) = hub_with_one();
        let unknown = Event::builder("t-x-c-zzz", "UID-A")
            .point(0.0, 0.0)
            .push(Element::new("whatever"))
            .build();

        assert!(matches!(
            handle(&hub, id, &unknown, CotTime::now()),
            ControlAction::Ignored(_)
        ));
        assert!(sent(&mut rx).is_none());
        assert_eq!(hub.len(), 1, "the subscription is still registered");
    }

    #[test]
    fn a_ping_from_a_connection_that_has_gone_is_not_a_failure() {
        let (hub, id, _rx) = hub_with_one();
        hub.unregister(id);

        assert!(matches!(
            handle(
                &hub,
                id,
                &msgs::ping("UID-A", CotTime::now()),
                CotTime::now()
            ),
            ControlAction::Ignored(_)
        ));
    }
}
