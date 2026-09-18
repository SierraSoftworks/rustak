//! Serialising an [`Event`] back to CoT XML.

use std::borrow::Cow;

use bytes::{BufMut, Bytes, BytesMut};
use quick_xml::escape::escape;

use super::DECLARATION;
use crate::detail::{Element, Node};
use crate::event::Event;

/// Serialises an event, declaration included, with no trailing newline.
#[must_use]
pub fn write(event: &Event) -> Bytes {
    let mut out = BytesMut::with_capacity(512);
    write_into(event, &mut out);
    out.freeze()
}

/// Serialises an event onto the end of an existing buffer.
pub fn write_into(event: &Event, dst: &mut BytesMut) {
    dst.extend_from_slice(DECLARATION.as_bytes());
    dst.put_u8(b'\n');

    dst.extend_from_slice(b"<event");
    attr(dst, "version", &event.version);
    attr(dst, "uid", &event.uid);
    attr(dst, "type", &event.r#type);
    attr_opt(dst, "how", event.how.as_deref());
    attr(dst, "time", &event.time.to_string());
    attr(dst, "start", &event.start.to_string());
    attr(dst, "stale", &event.stale.to_string());
    attr_opt(dst, "access", event.access.as_deref());
    attr_opt(dst, "qos", event.qos.as_deref());
    attr_opt(dst, "opex", event.opex.as_deref());
    attr_opt(dst, "caveat", event.caveat.as_deref());
    attr_opt(dst, "releasableTo", event.releasable_to.as_deref());
    for (name, value) in &event.extra_attrs {
        attr(dst, name, value);
    }
    dst.put_u8(b'>');

    dst.extend_from_slice(b"<point");
    attr(dst, "lat", &format_f64(event.point.lat));
    attr(dst, "lon", &format_f64(event.point.lon));
    attr(dst, "hae", &format_f64(event.point.hae));
    attr(dst, "ce", &format_f64(event.point.ce));
    attr(dst, "le", &format_f64(event.point.le));
    dst.extend_from_slice(b"/>");

    if !event.detail.is_empty() {
        dst.extend_from_slice(b"<detail>");
        write_nodes(&event.detail.nodes, dst);
        dst.extend_from_slice(b"</detail>");
    }

    dst.extend_from_slice(b"</event>");
}

/// Serialises detail children with no wrapping element.
///
/// This is the `xmlDetail` form the protobuf encoding carries: the children of
/// `<detail>` concatenated, without `<detail>` itself.
#[must_use]
pub fn write_fragment(nodes: &[Node]) -> String {
    let mut out = BytesMut::with_capacity(128);
    write_nodes(nodes, &mut out);
    // Every byte we wrote came from `str` input, so this cannot fail.
    String::from_utf8(out.to_vec()).unwrap_or_default()
}

/// Formats a double the way CoT consumers expect: shortest round-trip, always
/// with a decimal point, and never `NaN` or `inf`.
#[must_use]
pub fn format_f64(value: f64) -> String {
    if value.is_finite() {
        format!("{value:?}")
    } else {
        "0.0".to_owned()
    }
}

/// Writes ` name="value"`.
fn attr(dst: &mut BytesMut, name: &str, value: &str) {
    dst.put_u8(b' ');
    dst.extend_from_slice(strip_controls(name, false).as_bytes());
    dst.extend_from_slice(b"=\"");
    dst.extend_from_slice(escape(strip_controls(value, false)).as_bytes());
    dst.put_u8(b'"');
}

/// Writes ` name="value"` when there is a value.
fn attr_opt(dst: &mut BytesMut, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        attr(dst, name, value);
    }
}

fn write_nodes(nodes: &[Node], dst: &mut BytesMut) {
    for node in nodes {
        write_node(node, dst);
    }
}

fn write_node(node: &Node, dst: &mut BytesMut) {
    match node {
        Node::Element(element) => write_element(element, dst),
        Node::Text(text) => {
            dst.extend_from_slice(escape(strip_controls(text, true)).as_bytes());
        }
        Node::CData(text) => {
            // A CDATA section cannot contain `]]>`; splitting it across two
            // sections preserves the exact characters.
            dst.extend_from_slice(b"<![CDATA[");
            dst.extend_from_slice(
                strip_controls(text, true)
                    .replace("]]>", "]]]]><![CDATA[>")
                    .as_bytes(),
            );
            dst.extend_from_slice(b"]]>");
        }
        Node::Comment(text) => {
            // `--` may not appear inside a comment, and it may not end with `-`.
            let body = strip_controls(text, true).replace("--", "- -");
            dst.extend_from_slice(b"<!--");
            dst.extend_from_slice(body.as_bytes());
            if body.ends_with('-') {
                dst.put_u8(b' ');
            }
            dst.extend_from_slice(b"-->");
        }
    }
}

fn write_element(element: &Element, dst: &mut BytesMut) {
    let name = strip_controls(&element.name, false);
    dst.put_u8(b'<');
    dst.extend_from_slice(name.as_bytes());
    for (key, value) in &element.attrs {
        attr(dst, key, value);
    }
    if element.children.is_empty() {
        dst.extend_from_slice(b"/>");
        return;
    }
    dst.put_u8(b'>');
    write_nodes(&element.children, dst);
    dst.extend_from_slice(b"</");
    dst.extend_from_slice(name.as_bytes());
    dst.put_u8(b'>');
}

/// Removes the characters CloudTAK strips before parsing, plus the C0 controls
/// XML forbids outright.
///
/// `keep_ws` retains `\t` and `\n` for character data; attribute values never
/// keep them, because XML whitespace-normalises them on the way back in and a
/// silently changed value is worse than a dropped one. `\r` is always removed.
fn strip_controls(raw: &str, keep_ws: bool) -> Cow<'_, str> {
    let forbidden = |c: char| match c {
        '\t' | '\n' => !keep_ws,
        '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => true,
        _ => false,
    };
    if raw.contains(forbidden) {
        Cow::Owned(raw.chars().filter(|c| !forbidden(*c)).collect())
    } else {
        Cow::Borrowed(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::{Contact, Detail, TypedDetail};
    use crate::event::Point;
    use crate::time::CotTime;

    fn event() -> Event {
        let now = CotTime::from_millis(1_789_646_400_000);
        Event::builder("a-f-G-U-C", "UID-A")
            .how("m-g")
            .time(now)
            .stale_after(std::time::Duration::from_secs(60))
            .point_full(Point {
                lat: 51.5074,
                lon: -0.1278,
                hae: 35.0,
                ce: Point::UNKNOWN_CE,
                le: Point::UNKNOWN_LE,
            })
            .build()
    }

    fn rendered(event: &Event) -> String {
        String::from_utf8(write(event).to_vec()).unwrap()
    }

    #[test]
    fn the_envelope_matches_the_verified_tak_server_shape() {
        let text = rendered(&event());
        assert_eq!(
            text,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                "\n",
                r#"<event version="2.0" uid="UID-A" type="a-f-G-U-C" how="m-g" "#,
                r#"time="2026-09-17T12:00:00.000Z" start="2026-09-17T12:00:00.000Z" "#,
                r#"stale="2026-09-17T12:01:00.000Z">"#,
                r#"<point lat="51.5074" lon="-0.1278" hae="35.0" ce="9999999.0" le="9999999.0"/>"#,
                "</event>"
            )
        );
        assert!(!text.ends_with('\n'));
        assert!(!text.contains("<event/>"));
        assert!(!text.contains("<detail"));
    }

    #[test]
    fn detail_is_written_only_when_it_holds_something() {
        let mut with_detail = event();
        with_detail
            .detail
            .set(&Contact::new("ALPHA").with_endpoint("*:-1:stcp"));
        let text = rendered(&with_detail);
        assert!(
            text.contains(r#"<detail><contact callsign="ALPHA" endpoint="*:-1:stcp"/></detail>"#)
        );

        let empty = event();
        assert!(!rendered(&empty).contains("<detail"));
    }

    #[test]
    fn optional_attributes_follow_the_documented_order() {
        let mut event = event();
        event.access = Some("Unclassified".into());
        event.qos = Some("1-r-c".into());
        event.opex = Some("e".into());
        event.caveat = Some("FOUO".into());
        event.releasable_to = Some("GBR".into());
        event.extra_attrs.push(("vendor".into(), "rustak".into()));
        let text = rendered(&event);
        let tail = text.split_once("stale=").unwrap().1;
        assert!(tail.starts_with(
            r#""2026-09-17T12:01:00.000Z" access="Unclassified" qos="1-r-c" opex="e" caveat="FOUO" releasableTo="GBR" vendor="rustak">"#
        ));
    }

    #[test]
    fn values_are_escaped_and_control_characters_are_dropped() {
        let mut event = event();
        event.detail.push(
            crate::detail::Element::new("remarks")
                .attr("source", "A & \"B\" <C>\r\n")
                .with(Node::Text("line1\nline2\r\u{7}\u{9f}".into())),
        );
        let text = rendered(&event);
        assert!(text.contains(r#"source="A &amp; &quot;B&quot; &lt;C&gt;""#));
        assert!(text.contains("line1\nline2</remarks>"));
        assert!(!text.contains('\r'));
        assert!(!text.contains('\u{7}'));
        assert!(!text.contains('\u{9f}'));
    }

    #[test]
    fn childless_elements_are_self_closing_but_the_event_never_is() {
        let mut event = event();
        event
            .detail
            .push(crate::detail::Element::new("__forcedelete"));
        let text = rendered(&event);
        assert!(text.contains("<__forcedelete/>"));
        assert!(text.ends_with("</event>"));
    }

    #[test]
    fn cdata_and_comments_are_kept_safe() {
        let nodes = vec![
            Node::CData("a]]>b".into()),
            Node::Comment("before -- after -".into()),
        ];
        let text = write_fragment(&nodes);
        assert_eq!(
            text,
            "<![CDATA[a]]]]><![CDATA[>b]]><!--before - - after - -->"
        );
        assert!(!text.contains("--><![CDATA"));
    }

    #[test]
    fn fragments_carry_no_detail_wrapper() {
        let mut detail = Detail::new();
        detail.push(Contact::new("ALPHA").to_element());
        detail.push(crate::detail::Element::new("__group").attr("name", "Cyan"));
        assert_eq!(
            write_fragment(&detail.nodes),
            r#"<contact callsign="ALPHA"/><__group name="Cyan"/>"#
        );
        assert_eq!(write_fragment(&[]), "");
    }

    #[test]
    fn doubles_round_trip_and_never_render_as_nan() {
        assert_eq!(format_f64(0.0), "0.0");
        assert_eq!(format_f64(9_999_999.0), "9999999.0");
        assert_eq!(format_f64(-0.127_800_000_1), "-0.1278000001");
        assert_eq!(format_f64(f64::NAN), "0.0");
        assert_eq!(format_f64(f64::INFINITY), "0.0");
    }
}
