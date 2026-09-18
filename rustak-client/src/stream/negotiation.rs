//! The client side of TAK Protocol v1 negotiation.
//!
//! The server offers once (`t-x-takp-v`), the client asks once (`t-x-takp-q`)
//! and then **says nothing at all** until the answer (`t-x-takp-r`) arrives —
//! which is why this is a state machine rather than a pair of handlers. An
//! event written between the request and the response would be XML arriving at
//! a server that has already switched its reader to protobuf.
//!
//! The rules, from `compat/streaming.md` §4 and `research/07` §3.4:
//!
//! | In | Out |
//! |---|---|
//! | offer supporting version 1 | send the request, reusing the offer's uid |
//! | offer we cannot use, or negotiation switched off | stay XML for the life of the connection |
//! | response `true` | both directions switch to protobuf immediately |
//! | response `false` | stay XML |
//! | no response inside 60 s | stay XML — the silence is the documented answer |
//!
//! This module holds no timer and performs no I/O: [`TakStream`](super::TakStream)
//! owns both. Everything here is a pure transition, which is what makes the
//! table above testable without a socket.

use rustak_cot::{Event, negotiate};

/// Where a connection has got to in deciding its encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Negotiation {
    /// No offer has arrived yet. The connection speaks XML and may still
    /// switch.
    #[default]
    Waiting,

    /// A request is outstanding. The connection still speaks XML, but nothing
    /// may be written until the server answers.
    Requested,

    /// The server accepted: both directions speak TAK Protocol v1.
    Proto,

    /// Settled on XML — refused, timed out, never offered, or never asked for.
    /// This is the normal state of a CloudTAK connection.
    Xml,
}

impl Negotiation {
    /// Whether outbound events must be held back rather than written.
    #[must_use]
    pub const fn is_quiet(self) -> bool {
        matches!(self, Self::Requested)
    }

    /// Whether the encoding can still change.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Proto | Self::Xml)
    }
}

/// The client's negotiation state, and the server version it learned on the way.
#[derive(Clone, Debug)]
pub(crate) struct ClientNeg {
    state: Negotiation,
    enabled: bool,
    server_version: Option<String>,
}

impl ClientNeg {
    /// A connection that will ask for protobuf when offered it, or one that
    /// will not (`enabled = false`).
    pub(crate) const fn new(enabled: bool) -> Self {
        Self {
            state: Negotiation::Waiting,
            enabled,
            server_version: None,
        }
    }

    /// Where negotiation has got to.
    pub(crate) const fn state(&self) -> Negotiation {
        self.state
    }

    /// The `serverVersion` the offer carried, if it carried one.
    ///
    /// Read even when we decline the offer: it is the only place a TAK server
    /// announces its own version, and CloudTAK displays it for exactly that
    /// reason.
    pub(crate) fn server_version(&self) -> Option<&str> {
        self.server_version.as_deref()
    }

    /// Handles a `t-x-takp-v` offer, returning the request to send.
    ///
    /// Returns `None` when there is nothing to send — because we are not
    /// negotiating, because the offer does not support version 1, or because
    /// an offer has already been dealt with. A second offer is ignored: the
    /// server sends exactly one.
    pub(crate) fn on_announce(&mut self, event: &Event, now: rustak_cot::CotTime) -> Option<Event> {
        let announce = negotiate::parse_announce(event)?;

        if self.server_version.is_none() {
            self.server_version = announce.server_version.clone();
        }

        if self.state != Negotiation::Waiting {
            return None;
        }

        if !self.enabled || !announce.supports(negotiate::PROTO_VERSION) {
            // The silent fallback is the documented one: a client that cannot
            // use the offer simply carries on in XML.
            self.state = Negotiation::Xml;
            return None;
        }

        self.state = Negotiation::Requested;

        // The response is matched to the offer by uid, so the request reuses it.
        Some(negotiate::request(
            event.uid.clone(),
            negotiate::PROTO_VERSION,
            now,
        ))
    }

    /// Handles a `t-x-takp-r` response, returning the encoding to switch to.
    ///
    /// `None` means the message was not a usable response, or arrived when no
    /// request was outstanding; the connection is left exactly as it was.
    pub(crate) fn on_response(&mut self, event: &Event) -> Option<Negotiation> {
        if self.state != Negotiation::Requested {
            return None;
        }

        let accepted = negotiate::parse_response(event)?;

        self.state = match accepted {
            true => Negotiation::Proto,
            false => Negotiation::Xml,
        };

        Some(self.state)
    }

    /// Gives up waiting for a response: XML, for the life of the connection.
    pub(crate) fn expire(&mut self) -> Negotiation {
        if self.state == Negotiation::Requested {
            self.state = Negotiation::Xml;
        }

        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_cot::CotTime;

    const NOW: CotTime = CotTime::from_millis(1_789_646_400_000);

    fn offer(versions: &[u32]) -> Event {
        let mut event = negotiate::announce("NEG-1", "rustak-0.1.0", negotiate::API_VERSION, NOW);

        // `announce` offers version 1; rewrite the support element when a test
        // wants a server that offers something else.
        if versions != [negotiate::PROTO_VERSION] {
            let control = event
                .detail
                .find_mut("TakControl")
                .expect("the offer carries a TakControl");
            control.children.clear();
            for version in versions {
                control.push(
                    rustak_cot::Element::new("TakProtocolSupport")
                        .attr("version", version.to_string()),
                );
            }
        }

        event
    }

    #[test]
    fn an_offer_of_version_one_is_answered_with_a_request_reusing_its_uid() {
        // The uid is how the server matches its response to the offer; a fresh
        // one produces a response that never matches anything.
        let mut neg = ClientNeg::new(true);

        let request = neg.on_announce(&offer(&[1]), NOW).expect("we should ask");

        assert_eq!(request.r#type, rustak_cot::types::cot_type::TAKP_Q);
        assert_eq!(request.uid, "NEG-1");
        assert_eq!(negotiate::parse_request(&request), Some(1));
        assert_eq!(neg.state(), Negotiation::Requested);
        assert!(neg.state().is_quiet());
    }

    #[test]
    fn the_offer_is_where_the_server_version_comes_from() {
        let mut neg = ClientNeg::new(true);

        neg.on_announce(&offer(&[1]), NOW);

        assert_eq!(neg.server_version(), Some("rustak-0.1.0"));
    }

    #[test]
    fn a_client_that_is_not_negotiating_still_reads_the_version_and_stays_xml() {
        // What CloudTAK does, and what `negotiate = false` asks for.
        let mut neg = ClientNeg::new(false);

        assert!(neg.on_announce(&offer(&[1]), NOW).is_none());
        assert_eq!(neg.state(), Negotiation::Xml);
        assert_eq!(neg.server_version(), Some("rustak-0.1.0"));
        assert!(neg.state().is_settled());
    }

    #[test]
    fn an_offer_of_a_version_we_do_not_speak_settles_on_xml_silently() {
        let mut neg = ClientNeg::new(true);

        assert!(neg.on_announce(&offer(&[2, 3]), NOW).is_none());
        assert_eq!(neg.state(), Negotiation::Xml);
    }

    #[test]
    fn acceptance_and_refusal_are_the_two_endings() {
        for (accepted, expected) in [(true, Negotiation::Proto), (false, Negotiation::Xml)] {
            let mut neg = ClientNeg::new(true);
            neg.on_announce(&offer(&[1]), NOW);

            let response = negotiate::response("NEG-1", accepted, NOW);

            assert_eq!(neg.on_response(&response), Some(expected));
            assert_eq!(neg.state(), expected);
        }
    }

    #[test]
    fn a_response_nobody_asked_for_changes_nothing() {
        // A stray `t-x-takp-r` must not flip a connection into protobuf that
        // the server is still reading as XML.
        let mut neg = ClientNeg::new(true);

        assert!(
            neg.on_response(&negotiate::response("NEG-1", true, NOW))
                .is_none()
        );
        assert_eq!(neg.state(), Negotiation::Waiting);
    }

    #[test]
    fn a_second_offer_is_ignored_rather_than_asked_about_twice() {
        let mut neg = ClientNeg::new(true);
        neg.on_announce(&offer(&[1]), NOW);

        assert!(neg.on_announce(&offer(&[1]), NOW).is_none());
        assert_eq!(neg.state(), Negotiation::Requested);
    }

    #[test]
    fn silence_from_the_server_ends_in_xml_not_in_a_dropped_connection() {
        // `research/07` §3.4: the client waits out the offer's 60-second window
        // and then carries on in XML. That is the correct fallback, not a bug.
        let mut neg = ClientNeg::new(true);
        neg.on_announce(&offer(&[1]), NOW);

        assert_eq!(neg.expire(), Negotiation::Xml);
        assert_eq!(neg.expire(), Negotiation::Xml);
    }

    #[test]
    fn expiry_cannot_undo_a_completed_switch() {
        let mut neg = ClientNeg::new(true);
        neg.on_announce(&offer(&[1]), NOW);
        neg.on_response(&negotiate::response("NEG-1", true, NOW));

        assert_eq!(neg.expire(), Negotiation::Proto);
    }
}
