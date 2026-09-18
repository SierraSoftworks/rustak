//! An event serialised at most once per encoding, however many peers get it.
//!
//! A busy server fans one inbound message out to every reachable subscriber,
//! and those subscribers are a mix of XML and protobuf connections. Encoding
//! per recipient would serialise the same event dozens of times; encoding both
//! forms eagerly would pay for a protobuf encode on a server whose clients are
//! all CloudTAK.
//!
//! [`EncodedEvent`] does neither: each form is produced on first request and
//! cached, so wrapping the event in an `Arc` and handing it to every writer
//! costs one XML encode, at most one protobuf encode, and a clone of a
//! refcounted [`Bytes`] per recipient.

use std::sync::OnceLock;

use bytes::Bytes;

use super::{Frame, Mode};
use crate::event::Event;

/// An [`Event`] with its wire forms cached beside it.
///
/// Both accessors are `&self`: the cache is filled through a [`OnceLock`], so
/// an `Arc<EncodedEvent>` shared across connection tasks needs no lock of its
/// own and the value stays `Send + Sync`.
#[derive(Debug)]
pub struct EncodedEvent {
    event: Event,
    xml: OnceLock<Bytes>,
    proto: OnceLock<Bytes>,
}

impl EncodedEvent {
    /// Wraps an event, encoding nothing yet.
    #[must_use]
    pub const fn new(event: Event) -> Self {
        Self {
            event,
            xml: OnceLock::new(),
            proto: OnceLock::new(),
        }
    }

    /// The event this was built from.
    #[must_use]
    pub const fn event(&self) -> &Event {
        &self.event
    }

    /// The XML form: declaration, newline, `<event>…</event>`, no trailing
    /// newline — the exact bytes to put on an XML connection.
    #[must_use]
    pub fn xml(&self) -> &Bytes {
        self.xml.get_or_init(|| crate::xml::write(&self.event))
    }

    /// The protobuf form: a serialised `TakMessage`, *without* the `0xBF`
    /// magic byte and length prefix, which the codec adds when it writes the
    /// frame.
    #[must_use]
    pub fn proto(&self) -> &Bytes {
        self.proto
            .get_or_init(|| crate::proto::encode(&crate::proto::event_to_message(&self.event)))
    }

    /// The frame to hand a connection in this mode.
    ///
    /// The returned [`Bytes`] shares the cached buffer rather than copying it.
    #[must_use]
    pub fn frame(&self, mode: Mode) -> Frame {
        match mode {
            Mode::Xml => Frame::Xml(self.xml().clone()),
            Mode::Proto => Frame::Proto(self.proto().clone()),
        }
    }

    /// How many bytes this event's payload occupies in `mode`.
    ///
    /// For [`Mode::Proto`] this is the `TakMessage` length that decides
    /// whether a server must substitute a `b-f-t-r` pointer
    /// ([`MAX_PROTO_PAYLOAD`](super::MAX_PROTO_PAYLOAD)); the frame on the
    /// wire is a few bytes longer.
    #[must_use]
    pub fn len(&self, mode: Mode) -> usize {
        match mode {
            Mode::Xml => self.xml().len(),
            Mode::Proto => self.proto().len(),
        }
    }
}

impl From<Event> for EncodedEvent {
    fn from(event: Event) -> Self {
        Self::new(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::Contact;
    use crate::time::CotTime;

    fn sample() -> Event {
        Event::builder("a-f-G-U-C", "UID-ALPHA")
            .how("m-g")
            .point(51.5074, -0.1278)
            .time(CotTime::from_millis(1_789_646_400_000))
            .stale_after(std::time::Duration::from_secs(60))
            .typed(&Contact::new("ALPHA").with_endpoint("*:-1:stcp"))
            .build()
    }

    #[test]
    fn the_xml_form_is_exactly_what_the_writer_produces() {
        let event = sample();
        let encoded = EncodedEvent::new(event.clone());
        assert_eq!(encoded.xml(), &crate::xml::write(&event));
        assert_eq!(encoded.event(), &event);
    }

    #[test]
    fn each_form_is_encoded_once_and_shared_thereafter() {
        let encoded = EncodedEvent::new(sample());
        let first = encoded.xml().clone();
        let second = encoded.xml().clone();
        // Same allocation, not merely equal contents.
        assert_eq!(first.as_ptr(), second.as_ptr());

        let first = encoded.proto().clone();
        let second = encoded.proto().clone();
        assert_eq!(first.as_ptr(), second.as_ptr());
    }

    #[test]
    fn the_proto_form_round_trips_back_to_the_event() {
        let encoded = EncodedEvent::new(sample());
        let message = crate::proto::decode(encoded.proto()).expect("our own encoding");
        let decoded = crate::proto::message_to_event(message).expect("a cotEvent is present");
        assert_eq!(decoded.uid, encoded.event().uid);
        assert_eq!(decoded.callsign(), Some("ALPHA"));
    }

    #[test]
    fn frames_carry_the_cached_payload_for_the_mode() {
        let encoded = EncodedEvent::new(sample());
        assert_eq!(encoded.frame(Mode::Xml), Frame::Xml(encoded.xml().clone()));
        assert_eq!(
            encoded.frame(Mode::Proto),
            Frame::Proto(encoded.proto().clone())
        );
    }

    #[test]
    fn the_protobuf_form_is_the_smaller_one_for_a_typical_event() {
        let encoded = EncodedEvent::new(sample());
        assert_eq!(encoded.len(Mode::Xml), encoded.xml().len());
        assert_eq!(encoded.len(Mode::Proto), encoded.proto().len());
        assert!(
            encoded.len(Mode::Proto) < encoded.len(Mode::Xml),
            "protobuf {} should beat XML {}",
            encoded.len(Mode::Proto),
            encoded.len(Mode::Xml)
        );
    }

    #[test]
    fn an_encoded_event_can_be_shared_across_threads() {
        let shared = std::sync::Arc::new(EncodedEvent::from(sample()));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let shared = std::sync::Arc::clone(&shared);
                std::thread::spawn(move || shared.xml().len() + shared.proto().len())
            })
            .collect();
        let sizes: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("no panic"))
            .collect();
        assert!(sizes.iter().all(|size| *size == sizes[0]));
    }
}
