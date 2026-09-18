//! TAK Protocol v1: the generated message types and the conversion to and
//! from the [`Event`](crate::Event) model.
//!
//! The schema lives in `proto/tak_protocol_v1.proto` and is compiled by
//! `build.rs` with `protox` + `prost-build`, so no `protoc` binary is
//! needed anywhere in the build — including cross-compilation containers.
//!
//! # What the encoding costs
//!
//! The protobuf form is not a superset of the XML form, so a conversion is
//! lossy in two documented places:
//!
//! * `<event>` attributes rustak does not model
//!   ([`Event::extra_attrs`](crate::Event::extra_attrs)) have no field to
//!   live in and are dropped by [`event_to_message`]; and
//! * `<detail>` child order changes, because a decoder emits the typed
//!   sub-messages before the `xmlDetail` remainder.
//!
//! Everything else survives: unknown detail elements, their attributes,
//! text, CDATA and comments all travel verbatim inside `xmlDetail`, and
//! extension payloads are relayed untouched through
//! [`Event::proto_extensions`](crate::Event::proto_extensions).
//!
//! ```
//! use rustak_cot::{CotTime, Event, proto};
//! use rustak_cot::detail::Contact;
//!
//! let event = Event::builder("a-f-G-U-C", "UID-A")
//!     .how("m-g")
//!     .point(51.5074, -0.1278)
//!     .time(CotTime::from_millis(1_789_646_400_000))
//!     .typed(&Contact::new("ALPHA").with_endpoint("*:-1:stcp"))
//!     .build();
//!
//! let bytes = proto::encode(&proto::event_to_message(&event));
//! let decoded = proto::message_to_event(proto::decode(&bytes)?)?;
//! assert_eq!(decoded.callsign(), Some("ALPHA"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod from_proto;
mod to_proto;

use bytes::Bytes;
use prost::Message;

pub use from_proto::message_to_event;
pub use to_proto::event_to_message;

/// The generated types, kept in their own module so that the blanket
/// `allow` for machine-written code does not cover anything we hand-wrote.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
mod generated {
    include!(concat!(
        env!("OUT_DIR"),
        "/atakmap.commoncommo.protobuf.v1.rs"
    ));
}

pub use generated::detail::ExtensionEncodedDetail;
pub use generated::{
    Contact, CotEvent, Detail, Group, PrecisionLocation, Status, TakControl, TakMessage, Takv,
    Track,
};

/// Serialises a message into a fresh buffer.
///
/// This is the payload of a protocol frame; the `0xBF` magic byte and the
/// length varint are added by the codec, not here.
#[must_use]
pub fn encode(message: &TakMessage) -> Bytes {
    let mut buffer = bytes::BytesMut::with_capacity(message.encoded_len());
    // `BytesMut` grows on demand, so the only documented failure mode of
    // `encode` (insufficient capacity) cannot occur here.
    let _ = message.encode(&mut buffer);
    buffer.freeze()
}

/// Parses a frame payload back into a message.
///
/// # Errors
///
/// Returns [`prost::DecodeError`] when the bytes are not a well-formed
/// `TakMessage`. Unknown fields are skipped rather than rejected, so a peer
/// speaking a later revision of the schema still decodes.
pub fn decode(bytes: &[u8]) -> Result<TakMessage, prost::DecodeError> {
    TakMessage::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the protox/prost-build pipeline end to end: every field of the
    /// full schema survives an encode/decode round trip, with no `protoc`
    /// available anywhere in the build.
    #[test]
    fn tak_message_round_trips_through_the_wire_format() {
        let message = TakMessage {
            tak_control: Some(TakControl {
                min_proto_version: 1,
                max_proto_version: 2,
                contact_uid: "TEST-CONTACT-UID".to_string(),
                extension_ids: vec![7, 9],
            }),
            cot_event: Some(CotEvent {
                r#type: "a-f-G-U-C".to_string(),
                access: String::new(),
                qos: String::new(),
                opex: String::new(),
                uid: "TEST-UID".to_string(),
                send_time: 1_726_000_000_000,
                start_time: 1_726_000_000_000,
                stale_time: 1_726_000_060_000,
                how: "m-g".to_string(),
                lat: 51.5074,
                lon: -0.1278,
                hae: 35.0,
                ce: 9999999.0,
                le: 9999999.0,
                detail: Some(Detail {
                    xml_detail: "<uid Droid=\"ALPHA\"/>".to_string(),
                    contact: Some(Contact {
                        endpoint: "*:-1:stcp".to_string(),
                        callsign: "ALPHA".to_string(),
                    }),
                    group: Some(Group {
                        name: "Cyan".to_string(),
                        role: "Team Member".to_string(),
                    }),
                    precision_location: Some(PrecisionLocation {
                        geopointsrc: "GPS".to_string(),
                        altsrc: "GPS".to_string(),
                    }),
                    status: Some(Status { battery: 87 }),
                    takv: Some(Takv {
                        device: "Test Handset".to_string(),
                        platform: "rustak-test".to_string(),
                        os: "34".to_string(),
                        version: "0.1.0".to_string(),
                    }),
                    track: Some(Track {
                        speed: 1.5,
                        course: 270.0,
                    }),
                    extension_details: vec![ExtensionEncodedDetail {
                        extension_id: 42,
                        data: Bytes::from_static(b"\x00\xff opaque"),
                    }],
                }),
                caveat: "NONE".to_string(),
                releasable_to: "ALL".to_string(),
            }),
        };

        let bytes = encode(&message);
        let decoded = decode(&bytes).expect("decode what we just encoded");

        assert_eq!(decoded, message);
    }

    /// Field numbers are the part of this schema that has to match what other
    /// clients already send, so they are asserted against the wire bytes
    /// rather than trusted to survive an edit of the `.proto`.
    #[test]
    fn the_load_bearing_field_numbers_are_pinned() {
        let message = TakMessage {
            tak_control: None,
            cot_event: Some(CotEvent {
                uid: "U".to_string(),
                ..CotEvent::default()
            }),
        };
        // tag 2 (cotEvent), length-delimited -> 0x12; inside it tag 5 (uid),
        // length-delimited -> 0x2a.
        assert_eq!(&encode(&message)[..], b"\x12\x03\x2a\x01U");

        let detail = Detail {
            status: Some(Status { battery: 5 }),
            ..Detail::default()
        };
        // tag 5 (status) -> 0x2a, inside it tag 1 (battery), varint -> 0x08.
        assert_eq!(&detail.encode_to_vec()[..], b"\x2a\x02\x08\x05");
    }

    /// Numbers 3 and 4 of `TakMessage` are reserved for a server's own
    /// timestamps; we neither emit nor require them, and a message carrying
    /// them still decodes.
    #[test]
    fn reserved_takmessage_fields_are_skipped_not_rejected() {
        let mut bytes = encode(&TakMessage {
            tak_control: None,
            cot_event: Some(CotEvent {
                uid: "U".to_string(),
                ..CotEvent::default()
            }),
        })
        .to_vec();
        // tag 3, varint -> 0x18; tag 4, varint -> 0x20.
        bytes.extend_from_slice(b"\x18\x01\x20\x02");

        let decoded = decode(&bytes).expect("unknown fields are skipped");
        assert_eq!(decoded.cot_event.expect("event").uid, "U");
    }

    #[test]
    fn a_truncated_payload_is_an_error_not_a_panic() {
        let bytes = encode(&TakMessage {
            tak_control: None,
            cot_event: Some(CotEvent {
                uid: "LONG-UID".to_string(),
                ..CotEvent::default()
            }),
        });
        assert!(decode(&bytes[..bytes.len() - 2]).is_err());
    }
}
