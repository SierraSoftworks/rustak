//! The one protocol offer a connection ever makes.
//!
//! `compat/streaming.md` §4: the server sends exactly one `t-x-takp-v` after
//! the latest-SA replay, the client may answer once with `t-x-takp-q` reusing
//! the offer's uid, and the server answers `t-x-takp-r` — after which both ends
//! switch to protobuf framing immediately and no XML is ever written on that
//! socket again.
//!
//! # Saying nothing is a valid answer
//!
//! A request whose `TakRequest/@version` is missing or unreadable gets **no
//! reply at all**. The client times out after sixty seconds and stays in XML
//! for the life of the connection, which is the correct fallback rather than a
//! hang to be fixed: answering something the client cannot interpret is how a
//! connection ends up half-switched, with one side framing protobuf and the
//! other reading XML.
//!
//! CloudTAK never sends a request at all, and that is also normal: the offer is
//! harmless to it, it reads the server version out of it, and the connection
//! stays in XML forever.

use rustak_cot::codec::Mode;
use rustak_cot::types::cot_type;
use rustak_cot::{CotTime, Event, negotiate};

use crate::config::stream::NegotiationMode;
use crate::prelude::*;

/// Where a connection is in its one negotiation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NegState {
    /// The installation does not offer protobuf.
    Disabled,
    /// The offer has gone out and nothing has come back.
    Offered {
        /// The uid the client must echo.
        uid: String,
    },
    /// The client asked and was told yes; the connection is protobuf.
    Proto,
    /// The client asked for something we would not give, or never asked.
    Xml,
}

/// What handling a `t-x-takp-q` produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Intercepted {
    /// Write this answer, then switch both directions to protobuf.
    Accepted(Box<Event>),
    /// Write this answer and stay in XML.
    Refused(Box<Event>),
    /// Say nothing.
    Silent,
}

/// One connection's negotiation state machine.
#[derive(Clone, Debug)]
pub struct Negotiation {
    state: NegState,
    server_version: String,

    /// What the installation's compatibility switch says to answer.
    configured: NegotiationMode,
}

impl Negotiation {
    /// A negotiation in the mode the listener was configured with.
    ///
    /// The mode arrives on `ConnLimits::negotiate`, which `stream/mod.rs` fills
    /// from `[stream] negotiation` — or from [`NegotiationMode::Silent`] when
    /// `[stream.limits] negotiate_protobuf` is off, because an installation
    /// that does not offer protobuf makes no offer whatever the switch says.
    pub fn with_mode(mode: NegotiationMode, server_version: impl Into<String>) -> Self {
        Self {
            state: if mode == NegotiationMode::Silent {
                NegState::Disabled
            } else {
                NegState::Xml
            },
            server_version: server_version.into(),
            configured: mode,
        }
    }

    /// Where the negotiation has got to.
    pub fn state(&self) -> &NegState {
        &self.state
    }

    /// Which encoding this connection is written in.
    pub fn mode(&self) -> Mode {
        match self.state {
            NegState::Proto => Mode::Proto,
            _ => Mode::Xml,
        }
    }

    /// The single offer, if this connection makes one.
    ///
    /// `uid` is kept, because the client echoes it and the answer must reuse
    /// it. Calling this twice returns [`None`] the second time: "exactly one"
    /// is part of the contract, and a client that saw two would have no way to
    /// know which one its answer belonged to.
    pub fn offer(&mut self, uid: impl Into<String>, now: CotTime) -> Option<Event> {
        if self.state != NegState::Xml {
            return None;
        }

        let uid = uid.into();
        let event = negotiate::announce(
            uid.clone(),
            self.server_version.clone(),
            negotiate::API_VERSION,
            now,
        );

        self.state = NegState::Offered { uid };

        Some(event)
    }

    /// Handles a message that may be the client's answer.
    ///
    /// Returns [`None`] for anything that is not a `t-x-takp-q`, which the
    /// caller then routes normally.
    pub fn on_event(&mut self, event: &Event, now: CotTime) -> Option<Intercepted> {
        if event.r#type != cot_type::TAKP_Q {
            return None;
        }

        let NegState::Offered { uid } = self.state.clone() else {
            debug!(
                state = ?self.state,
                "A client asked to negotiate outside the one window it had; ignoring it."
            );

            return Some(Intercepted::Silent);
        };

        let Some(version) = negotiate::parse_request(event) else {
            // Deliberately no answer: see the module documentation.
            debug!("A protocol request named no version we could read; staying in XML silently.");

            return Some(Intercepted::Silent);
        };

        if version != negotiate::PROTO_VERSION || self.configured == NegotiationMode::Refuse {
            self.state = NegState::Xml;

            return Some(Intercepted::Refused(Box::new(negotiate::response(
                uid, false, now,
            ))));
        }

        let answer = negotiate::response(uid, true, now);
        self.state = NegState::Proto;

        Some(Intercepted::Accepted(Box::new(answer)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> CotTime {
        CotTime::from_millis(1_789_646_400_000)
    }

    /// A negotiation in the mode every installation runs.
    fn accepting() -> Negotiation {
        Negotiation::with_mode(NegotiationMode::Accept, "rustak-0.1.0")
    }

    #[test]
    fn a_connection_offers_protobuf_exactly_once() {
        let mut negotiation = accepting();

        let offer = negotiation.offer("neg-1", now()).expect("the one offer");

        assert_eq!(offer.r#type, cot_type::TAKP_V);
        assert_eq!(offer.uid, "neg-1");
        assert!(
            negotiation.offer("neg-2", now()).is_none(),
            "a second offer would leave the client unable to tell which it answered",
        );
    }

    #[test]
    fn an_installation_that_does_not_offer_protobuf_says_nothing() {
        // `negotiate_protobuf = false` is what `stream/mod.rs` turns into
        // `Silent` before the connection is built.
        let mut negotiation = Negotiation::with_mode(NegotiationMode::Silent, "rustak-0.1.0");

        assert!(negotiation.offer("neg-1", now()).is_none());
        assert_eq!(negotiation.mode(), Mode::Xml);
    }

    #[test]
    fn the_silent_mode_never_offers_at_all() {
        // What `negotiate-silent` drives: ATAK waits sixty seconds for an offer
        // that never comes and continues in XML, keeping the connection.
        let mut negotiation = Negotiation::with_mode(NegotiationMode::Silent, "rustak-0.1.0");

        assert!(negotiation.offer("neg-1", now()).is_none());
        assert_eq!(negotiation.state(), &NegState::Disabled);
        assert_eq!(negotiation.mode(), Mode::Xml);
    }

    #[test]
    fn the_refuse_mode_offers_and_then_says_no() {
        // What `negotiate-refused` drives: the offer goes out, the client asks
        // for the version we do speak, and it is refused anyway — which is the
        // only way to reach ATAK's `using xml only` path from a working server.
        let mut negotiation = Negotiation::with_mode(NegotiationMode::Refuse, "rustak-0.1.0");

        let offer = negotiation
            .offer("neg-1", now())
            .expect("refusing still offers");

        assert_eq!(offer.r#type, cot_type::TAKP_V);

        let request = negotiate::request("neg-1", negotiate::PROTO_VERSION, now());
        let Some(Intercepted::Refused(answer)) = negotiation.on_event(&request, now()) else {
            panic!("the refusing mode must refuse a version 1 request");
        };

        assert_eq!(answer.r#type, cot_type::TAKP_R);
        assert_eq!(answer.uid, "neg-1", "the answer reuses the offer's uid");
        assert_eq!(negotiate::parse_response(&answer), Some(false));
        assert_eq!(
            negotiation.mode(),
            Mode::Xml,
            "a refused connection stays in XML for the life of the socket",
        );
    }

    #[test]
    fn a_request_for_version_one_switches_the_connection() {
        let mut negotiation = accepting();
        negotiation.offer("neg-1", now());

        let request = negotiate::request("neg-1", negotiate::PROTO_VERSION, now());
        let Some(Intercepted::Accepted(answer)) = negotiation.on_event(&request, now()) else {
            panic!("a version 1 request should be accepted");
        };

        assert_eq!(answer.r#type, cot_type::TAKP_R);
        assert_eq!(answer.uid, "neg-1", "the answer reuses the offer's uid");
        assert_eq!(negotiate::parse_response(&answer), Some(true));
        assert_eq!(negotiation.mode(), Mode::Proto);
    }

    #[test]
    fn a_request_for_a_version_we_do_not_speak_is_refused_and_stays_xml() {
        let mut negotiation = accepting();
        negotiation.offer("neg-1", now());

        let request = negotiate::request("neg-1", 7, now());
        let Some(Intercepted::Refused(answer)) = negotiation.on_event(&request, now()) else {
            panic!("an unknown version should be refused");
        };

        assert_eq!(negotiate::parse_response(&answer), Some(false));
        assert_eq!(negotiation.mode(), Mode::Xml);
    }

    #[test]
    fn a_request_with_no_readable_version_gets_no_answer_at_all() {
        // The client then times out at sixty seconds and stays in XML, which is
        // the correct fallback rather than a hang to be fixed.
        let mut negotiation = accepting();
        negotiation.offer("neg-1", now());

        let malformed = Event::builder(cot_type::TAKP_Q, "neg-1")
            .point_full(negotiate::POINT)
            .build();

        assert_eq!(
            negotiation.on_event(&malformed, now()),
            Some(Intercepted::Silent)
        );
        assert_eq!(negotiation.mode(), Mode::Xml);
    }

    #[test]
    fn a_request_before_the_offer_is_ignored() {
        let mut negotiation = accepting();
        let request = negotiate::request("neg-1", 1, now());

        assert_eq!(
            negotiation.on_event(&request, now()),
            Some(Intercepted::Silent)
        );
        assert_eq!(negotiation.mode(), Mode::Xml);
    }

    #[test]
    fn a_second_request_after_the_switch_changes_nothing() {
        let mut negotiation = accepting();
        negotiation.offer("neg-1", now());
        let request = negotiate::request("neg-1", 1, now());
        negotiation.on_event(&request, now());

        assert_eq!(
            negotiation.on_event(&request, now()),
            Some(Intercepted::Silent)
        );
        assert_eq!(negotiation.mode(), Mode::Proto);
    }

    #[test]
    fn an_ordinary_message_is_not_the_negotiations_business() {
        let mut negotiation = accepting();
        negotiation.offer("neg-1", now());

        let sa = Event::builder("a-f-G-U-C", "UID-A").point(0.0, 0.0).build();

        assert!(negotiation.on_event(&sa, now()).is_none());
    }
}
