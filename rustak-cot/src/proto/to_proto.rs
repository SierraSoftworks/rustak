//! [`Event`] to [`TakMessage`].
//!
//! The only interesting part is which `<detail>` children get promoted to a
//! typed sub-message. The rule is deliberately **strict**: an element is
//! promoted only when
//!
//! 1. exactly one element of that name sits at the top of `<detail>`,
//! 2. its attribute names are exactly the set the sub-message models,
//! 3. it has no children, and
//! 4. every numeric attribute parses.
//!
//! Anything else stays in `xmlDetail` verbatim. Being strict costs nothing —
//! a decoder that reads `xmlDetail` sees the same element it would have seen
//! typed — while being lenient would silently rewrite a peer's message: a
//! second `<contact>` would vanish, an unmodelled attribute would be dropped,
//! and `battery="full"` would arrive as `battery="0"`.

use crate::detail::{
    Contact, Detail, Group, Node, PrecisionLocation, Status, StrictDetail, Takv, Track, TypedDetail,
};
use crate::proto::{CotEvent, ExtensionEncodedDetail, TakMessage};
use crate::time::CotTime;
use crate::{Event, proto, xml};

/// Converts an event into the message a protocol frame carries.
///
/// `takControl` is never populated: rustak negotiates in XML, and no client
/// requires the protobuf control path.
///
/// Two things do not survive, both documented on the [module](crate::proto):
/// [`Event::extra_attrs`] has no field to live in, and `<detail>` child order
/// changes when a typed element was not already first.
#[must_use]
pub fn event_to_message(event: &Event) -> TakMessage {
    TakMessage {
        tak_control: None,
        cot_event: Some(CotEvent {
            r#type: event.r#type.clone(),
            access: event.access.clone().unwrap_or_default(),
            qos: event.qos.clone().unwrap_or_default(),
            opex: event.opex.clone().unwrap_or_default(),
            uid: event.uid.clone(),
            send_time: millis(event.time),
            start_time: millis(event.start),
            stale_time: millis(event.stale),
            how: event.how.clone().unwrap_or_default(),
            lat: event.point.lat,
            lon: event.point.lon,
            hae: event.point.hae,
            ce: event.point.ce,
            le: event.point.le,
            detail: detail(event),
            caveat: event.caveat.clone().unwrap_or_default(),
            releasable_to: event.releasable_to.clone().unwrap_or_default(),
        }),
    }
}

/// Milliseconds since the epoch, clamped at zero.
///
/// The field is a `uint64`, so a pre-epoch time has no representation; no CoT
/// message legitimately carries one.
fn millis(time: CotTime) -> u64 {
    u64::try_from(time.millis()).unwrap_or_default()
}

/// The `detail` field, or [`None`] when there is nothing at all to say.
fn detail(event: &Event) -> Option<proto::Detail> {
    if event.detail.is_empty() && event.proto_extensions.is_empty() {
        return None;
    }

    let (mut out, rest) = split_detail(&event.detail);
    out.xml_detail = xml::write_fragment(&rest);
    out.extension_details = event
        .proto_extensions
        .iter()
        .map(|(extension_id, data)| ExtensionEncodedDetail {
            extension_id: *extension_id,
            data: data.clone(),
        })
        .collect();
    Some(out)
}

/// Splits a detail tree into the typed sub-messages and the nodes that stay
/// as raw XML, in their original order.
fn split_detail(detail: &Detail) -> (proto::Detail, Vec<Node>) {
    let mut out = proto::Detail::default();
    let mut promoted: Vec<&'static str> = Vec::new();

    if let Some(value) = promote::<Contact>(detail) {
        out.contact = Some(proto::Contact {
            endpoint: value.endpoint.unwrap_or_default(),
            callsign: value.callsign,
        });
        promoted.push(Contact::NAME);
    }
    if let Some(value) = promote::<Group>(detail) {
        out.group = Some(proto::Group {
            name: value.name,
            role: value.role,
        });
        promoted.push(Group::NAME);
    }
    if let Some(value) = promote::<PrecisionLocation>(detail) {
        out.precision_location = Some(proto::PrecisionLocation {
            geopointsrc: value.geopointsrc,
            altsrc: value.altsrc,
        });
        promoted.push(PrecisionLocation::NAME);
    }
    if let Some(value) = promote::<Status>(detail) {
        out.status = Some(proto::Status {
            battery: value.battery,
        });
        promoted.push(Status::NAME);
    }
    if let Some(value) = promote::<Takv>(detail) {
        out.takv = Some(proto::Takv {
            device: value.device,
            platform: value.platform,
            os: value.os,
            version: value.version,
        });
        promoted.push(Takv::NAME);
    }
    if let Some(value) = promote::<Track>(detail) {
        out.track = Some(proto::Track {
            speed: value.speed,
            course: value.course,
        });
        promoted.push(Track::NAME);
    }

    let rest = detail
        .nodes
        .iter()
        .filter(|node| match node {
            Node::Element(element) => !promoted.contains(&element.name.as_str()),
            _ => true,
        })
        .cloned()
        .collect();

    (out, rest)
}

/// Reads a typed view, but only when the element is unambiguous and matches
/// the sub-message shape exactly.
fn promote<T: StrictDetail>(detail: &Detail) -> Option<T> {
    if detail.count(T::NAME) != 1 {
        return None;
    }
    T::strict(detail.find(T::NAME)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::Element;
    use pretty_assertions::assert_eq;

    fn event_with(detail: Detail) -> Event {
        let mut event = Event::builder("a-f-G-U-C", "UID-A")
            .how("m-g")
            .time(CotTime::from_millis(1_789_646_400_000))
            .point(51.5074, -0.1278)
            .detail(detail)
            .build();
        event.start = event.time;
        event.stale = CotTime::from_millis(1_789_646_460_000);
        event
    }

    fn convert(detail: Detail) -> proto::Detail {
        event_to_message(&event_with(detail))
            .cot_event
            .expect("an event was supplied")
            .detail
            .expect("the detail is not empty")
    }

    #[test]
    fn the_envelope_and_point_map_field_for_field() {
        let event = event_with(Detail::new());
        let cot = event_to_message(&event).cot_event.expect("event");

        assert_eq!(cot.r#type, "a-f-G-U-C");
        assert_eq!(cot.uid, "UID-A");
        assert_eq!(cot.how, "m-g");
        assert_eq!(cot.send_time, 1_789_646_400_000);
        assert_eq!(cot.start_time, 1_789_646_400_000);
        assert_eq!(cot.stale_time, 1_789_646_460_000);
        assert!((cot.lat - 51.5074).abs() < f64::EPSILON);
        assert!((cot.lon + 0.1278).abs() < f64::EPSILON);
        assert_eq!(cot.access, "");
        assert_eq!(cot.caveat, "");
    }

    #[test]
    fn an_empty_detail_produces_no_detail_message_at_all() {
        let cot = event_to_message(&event_with(Detail::new()))
            .cot_event
            .expect("event");
        assert_eq!(cot.detail, None);
    }

    #[test]
    fn every_typed_element_is_promoted_and_leaves_no_xml_detail() {
        let detail = Detail::from_iter([
            Element::new("contact")
                .attr("callsign", "ALPHA")
                .attr("endpoint", "*:-1:stcp"),
            Element::new("__group")
                .attr("name", "Cyan")
                .attr("role", "Team Member"),
            Element::new("precisionlocation")
                .attr("geopointsrc", "GPS")
                .attr("altsrc", "GPS"),
            Element::new("status").attr("battery", "87"),
            Element::new("takv")
                .attr("device", "Handset")
                .attr("platform", "rustak")
                .attr("os", "34")
                .attr("version", "0.1.0"),
            Element::new("track")
                .attr("speed", "1.5")
                .attr("course", "270"),
        ]);

        let out = convert(detail);

        assert_eq!(out.xml_detail, "", "nothing should be left over");
        assert_eq!(
            out.contact,
            Some(proto::Contact {
                endpoint: "*:-1:stcp".into(),
                callsign: "ALPHA".into(),
            })
        );
        assert_eq!(out.status, Some(proto::Status { battery: 87 }));
        assert_eq!(
            out.track,
            Some(proto::Track {
                speed: 1.5,
                course: 270.0
            })
        );
    }

    #[test]
    fn a_contact_without_an_endpoint_is_still_promoted() {
        let out = convert(Detail::from_iter([
            Element::new("contact").attr("callsign", "ALPHA")
        ]));
        assert_eq!(out.contact.expect("promoted").endpoint, "");
        assert_eq!(out.xml_detail, "");
    }

    #[test]
    fn an_extra_attribute_keeps_the_whole_element_in_xml_detail() {
        let out = convert(Detail::from_iter([Element::new("contact")
            .attr("callsign", "ALPHA")
            .attr("phone", "555")]));

        assert_eq!(out.contact, None);
        assert_eq!(out.xml_detail, r#"<contact callsign="ALPHA" phone="555"/>"#);
    }

    #[test]
    fn a_missing_attribute_keeps_the_whole_element_in_xml_detail() {
        let out = convert(Detail::from_iter([
            Element::new("__group").attr("name", "Cyan")
        ]));

        assert_eq!(out.group, None);
        assert_eq!(out.xml_detail, r#"<__group name="Cyan"/>"#);
    }

    #[test]
    fn a_child_element_keeps_the_whole_element_in_xml_detail() {
        let out = convert(Detail::from_iter([Element::new("status")
            .attr("battery", "50")
            .with(Element::new("note"))]));

        assert_eq!(out.status, None);
        assert_eq!(out.xml_detail, r#"<status battery="50"><note/></status>"#);
    }

    #[test]
    fn a_duplicate_element_is_ambiguous_so_neither_copy_is_promoted() {
        let out = convert(Detail::from_iter([
            Element::new("contact").attr("callsign", "ALPHA"),
            Element::new("contact").attr("callsign", "BRAVO"),
        ]));

        assert_eq!(out.contact, None);
        assert_eq!(
            out.xml_detail,
            r#"<contact callsign="ALPHA"/><contact callsign="BRAVO"/>"#
        );
    }

    #[test]
    fn an_unparsable_number_is_never_coerced_to_zero() {
        let out = convert(Detail::from_iter([
            Element::new("status").attr("battery", "full"),
            Element::new("track")
                .attr("speed", "fast")
                .attr("course", "270"),
        ]));

        assert_eq!(out.status, None, "battery=full must not become battery=0");
        assert_eq!(out.track, None);
        assert!(out.xml_detail.contains(r#"battery="full""#));
        assert!(out.xml_detail.contains(r#"speed="fast""#));
    }

    #[test]
    fn unpromoted_nodes_keep_their_order_text_and_comments() {
        let mut detail = Detail::from_iter([Element::new("uid").attr("Droid", "ALPHA")]);
        detail.push_node(Node::Comment(" hand written ".into()));
        detail.push(Element::new("contact").attr("callsign", "ALPHA"));
        detail.push_node(Node::CData("a ]] b".into()));

        let out = convert(detail);

        assert_eq!(out.contact.expect("promoted").callsign, "ALPHA");
        assert_eq!(
            out.xml_detail,
            r#"<uid Droid="ALPHA"/><!-- hand written --><![CDATA[a ]] b]]>"#
        );
    }

    #[test]
    fn extension_payloads_are_carried_through_untouched() {
        let mut event = event_with(Detail::new());
        event
            .proto_extensions
            .push((42, bytes::Bytes::from_static(b"\x00\xffopaque")));

        let out = event_to_message(&event)
            .cot_event
            .expect("event")
            .detail
            .expect("extensions alone justify a detail message");

        assert_eq!(out.xml_detail, "");
        assert_eq!(out.extension_details.len(), 1);
        assert_eq!(out.extension_details[0].extension_id, 42);
    }

    #[test]
    fn a_pre_epoch_time_clamps_rather_than_wrapping() {
        let mut event = event_with(Detail::new());
        event.time = CotTime::from_millis(-1);
        assert_eq!(
            event_to_message(&event).cot_event.expect("event").send_time,
            0
        );
    }
}
