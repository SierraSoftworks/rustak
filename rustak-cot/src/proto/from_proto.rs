//! [`TakMessage`] to [`Event`].
//!
//! Decoding is the mirror of [`to_proto`](super::to_proto) with one rule that
//! is not symmetric: the typed sub-messages are emitted **first**, and a
//! same-named element inside `xmlDetail` **wins** over the typed one.
//!
//! That tie-break matters because the two forms are not equally expressive.
//! An encoder only promotes an element it can represent exactly, so a
//! `<contact>` that survived in `xmlDetail` is one carrying something the
//! sub-message cannot hold — an extra attribute, a child, a second copy. If
//! the typed field is also set, the sender said two different things; the
//! richer statement is the one to keep.

use crate::detail::{
    Contact, Detail, Element, Group, Node, PrecisionLocation, Status, Takv, Track, TypedDetail,
};
use crate::error::ConvertError;
use crate::event::{Point, VERSION};
use crate::proto::TakMessage;
use crate::time::CotTime;
use crate::{Event, xml};

/// Converts a decoded message back into an event.
///
/// # Errors
///
/// * [`ConvertError::NoCotEvent`] when the message carries only a
///   `takControl` — there is no event to build.
/// * [`ConvertError::BadXmlDetail`] when the `xmlDetail` fragment is not
///   well-formed XML. The typed sub-messages are *not* salvaged in that case:
///   a peer that sent a broken fragment sent a broken message.
pub fn message_to_event(message: TakMessage) -> Result<Event, ConvertError> {
    let source = message.cot_event.ok_or(ConvertError::NoCotEvent)?;
    let proto_detail = source.detail.unwrap_or_default();

    let mut nodes = xml::parse_fragment(&proto_detail.xml_detail)?;
    let mut typed = typed_elements(&proto_detail, &nodes);
    typed.append(&mut nodes);

    Ok(Event {
        version: VERSION.to_owned(),
        uid: source.uid,
        r#type: source.r#type,
        how: set(source.how),
        time: time(source.send_time),
        start: time(source.start_time),
        stale: time(source.stale_time),
        access: set(source.access),
        qos: set(source.qos),
        opex: set(source.opex),
        caveat: set(source.caveat),
        releasable_to: set(source.releasable_to),
        extra_attrs: Vec::new(),
        point: Point {
            lat: source.lat,
            lon: source.lon,
            hae: source.hae,
            ce: source.ce,
            le: source.le,
        },
        detail: Detail { nodes: typed },
        proto_extensions: proto_detail
            .extension_details
            .into_iter()
            .map(|extension| (extension.extension_id, extension.data))
            .collect(),
    })
}

/// The typed sub-messages rendered back to elements, skipping any name the
/// `xmlDetail` fragment already claims.
fn typed_elements(detail: &crate::proto::Detail, fragment: &[Node]) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut push = |name: &str, element: Element| {
        if !claims(fragment, name) {
            nodes.push(Node::Element(element));
        }
    };

    if let Some(value) = detail.contact.clone() {
        push(
            Contact::NAME,
            Contact {
                callsign: value.callsign,
                endpoint: set(value.endpoint),
                extra: Vec::new(),
            }
            .to_element(),
        );
    }
    if let Some(value) = detail.group.clone() {
        push(Group::NAME, Group::new(value.name, value.role).to_element());
    }
    if let Some(value) = detail.precision_location.clone() {
        push(
            PrecisionLocation::NAME,
            PrecisionLocation::new(value.geopointsrc, value.altsrc).to_element(),
        );
    }
    if let Some(value) = detail.status {
        push(Status::NAME, Status::new(value.battery).to_element());
    }
    if let Some(value) = detail.takv.clone() {
        push(
            Takv::NAME,
            Takv {
                device: value.device,
                platform: value.platform,
                os: value.os,
                version: value.version,
                extra: Vec::new(),
            }
            .to_element(),
        );
    }
    if let Some(value) = detail.track {
        push(
            Track::NAME,
            Track::new(value.speed, value.course).to_element(),
        );
    }

    nodes
}

/// Whether the parsed fragment already has a top-level element of this name.
fn claims(fragment: &[Node], name: &str) -> bool {
    fragment
        .iter()
        .any(|node| matches!(node, Node::Element(element) if element.name == name))
}

/// A proto3 string field with no value is indistinguishable from an absent
/// one, so an empty string decodes as [`None`] rather than `Some("")`.
fn set(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// Milliseconds since the epoch, saturating rather than wrapping on a value
/// no clock could have produced.
fn time(millis: u64) -> CotTime {
    CotTime::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, CotEvent, ExtensionEncodedDetail};
    use pretty_assertions::assert_eq;

    fn message(detail: Option<proto::Detail>) -> TakMessage {
        TakMessage {
            tak_control: None,
            cot_event: Some(CotEvent {
                r#type: "a-f-G-U-C".into(),
                uid: "UID-A".into(),
                send_time: 1_789_646_400_000,
                start_time: 1_789_646_400_000,
                stale_time: 1_789_646_460_000,
                how: "m-g".into(),
                lat: 51.5074,
                lon: -0.1278,
                hae: 35.0,
                ce: 9_999_999.0,
                le: 9_999_999.0,
                detail,
                ..CotEvent::default()
            }),
        }
    }

    fn convert(detail: Option<proto::Detail>) -> Event {
        message_to_event(message(detail)).expect("a cotEvent is present")
    }

    #[test]
    fn a_control_only_message_has_no_event_to_convert() {
        let message = TakMessage {
            tak_control: Some(proto::TakControl::default()),
            cot_event: None,
        };
        assert!(matches!(
            message_to_event(message),
            Err(ConvertError::NoCotEvent)
        ));
    }

    #[test]
    fn the_envelope_and_point_map_back_field_for_field() {
        let event = convert(None);

        assert_eq!(event.version, "2.0");
        assert_eq!(event.uid, "UID-A");
        assert_eq!(event.r#type, "a-f-G-U-C");
        assert_eq!(event.how.as_deref(), Some("m-g"));
        assert_eq!(event.time.millis(), 1_789_646_400_000);
        assert_eq!(event.stale.millis(), 1_789_646_460_000);
        assert!(event.detail.is_empty());
        assert!(event.extra_attrs.is_empty());
    }

    #[test]
    fn an_unset_string_field_decodes_as_absent_not_as_empty() {
        let mut message = message(None);
        let cot = message.cot_event.as_mut().expect("event");
        cot.how = String::new();
        cot.caveat = String::new();

        let event = message_to_event(message).expect("event");
        assert_eq!(event.how, None);
        assert_eq!(event.caveat, None);
    }

    #[test]
    fn typed_sub_messages_come_back_as_elements_in_a_fixed_order() {
        let event = convert(Some(proto::Detail {
            contact: Some(proto::Contact {
                endpoint: "*:-1:stcp".into(),
                callsign: "ALPHA".into(),
            }),
            group: Some(proto::Group {
                name: "Cyan".into(),
                role: "Team Member".into(),
            }),
            status: Some(proto::Status { battery: 87 }),
            track: Some(proto::Track {
                speed: 1.5,
                course: 270.0,
            }),
            ..proto::Detail::default()
        }));

        let names: Vec<&str> = event
            .detail
            .elements()
            .map(|element| element.name.as_str())
            .collect();
        assert_eq!(names, ["contact", "__group", "status", "track"]);
        assert_eq!(event.callsign(), Some("ALPHA"));
        assert_eq!(
            xml::write_fragment(&event.detail.nodes),
            concat!(
                r#"<contact callsign="ALPHA" endpoint="*:-1:stcp"/>"#,
                r#"<__group name="Cyan" role="Team Member"/>"#,
                r#"<status battery="87"/>"#,
                r#"<track speed="1.5" course="270.0"/>"#,
            )
        );
    }

    #[test]
    fn an_empty_endpoint_is_omitted_rather_than_written_as_blank() {
        let event = convert(Some(proto::Detail {
            contact: Some(proto::Contact {
                endpoint: String::new(),
                callsign: "ALPHA".into(),
            }),
            ..proto::Detail::default()
        }));

        assert_eq!(
            xml::write_fragment(&event.detail.nodes),
            r#"<contact callsign="ALPHA"/>"#
        );
    }

    #[test]
    fn an_xml_detail_element_wins_over_the_typed_one_of_the_same_name() {
        let event = convert(Some(proto::Detail {
            xml_detail: r#"<contact callsign="BRAVO" phone="555"/><uid Droid="BRAVO"/>"#.into(),
            contact: Some(proto::Contact {
                endpoint: "*:-1:stcp".into(),
                callsign: "ALPHA".into(),
            }),
            status: Some(proto::Status { battery: 87 }),
            ..proto::Detail::default()
        }));

        assert_eq!(event.detail.count("contact"), 1);
        assert_eq!(event.callsign(), Some("BRAVO"));
        assert_eq!(
            xml::write_fragment(&event.detail.nodes),
            concat!(
                r#"<status battery="87"/>"#,
                r#"<contact callsign="BRAVO" phone="555"/>"#,
                r#"<uid Droid="BRAVO"/>"#,
            ),
            "the typed elements still lead, minus the one xmlDetail claimed"
        );
    }

    #[test]
    fn a_malformed_xml_detail_fails_the_whole_message() {
        let result = message_to_event(message(Some(proto::Detail {
            xml_detail: "<a></mismatched>".into(),
            ..proto::Detail::default()
        })));
        assert!(matches!(result, Err(ConvertError::BadXmlDetail(_))));
    }

    #[test]
    fn extension_payloads_are_kept_opaquely_on_the_event() {
        let event = convert(Some(proto::Detail {
            extension_details: vec![
                ExtensionEncodedDetail {
                    extension_id: 42,
                    data: bytes::Bytes::from_static(b"\x00\xffopaque"),
                },
                ExtensionEncodedDetail {
                    extension_id: 7,
                    data: bytes::Bytes::new(),
                },
            ],
            ..proto::Detail::default()
        }));

        assert_eq!(event.proto_extensions.len(), 2);
        assert_eq!(event.proto_extensions[0].0, 42);
        assert_eq!(&event.proto_extensions[0].1[..], b"\x00\xffopaque");
        assert!(
            event.detail.is_empty(),
            "extensions are not rendered as XML"
        );
    }

    #[test]
    fn a_detail_message_that_is_entirely_empty_produces_an_empty_detail() {
        let event = convert(Some(proto::Detail::default()));
        assert!(event.detail.is_empty());
    }

    #[test]
    fn text_and_comments_in_xml_detail_survive() {
        let event = convert(Some(proto::Detail {
            xml_detail: "<a/>text<!-- note --><![CDATA[raw]]>".into(),
            ..proto::Detail::default()
        }));

        assert_eq!(event.detail.nodes.len(), 4);
        assert_eq!(
            xml::write_fragment(&event.detail.nodes),
            "<a/>text<!-- note --><![CDATA[raw]]>"
        );
    }
}
