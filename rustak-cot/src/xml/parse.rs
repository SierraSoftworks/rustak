//! Parsing CoT XML into an [`Event`].

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event as XmlEvent};

use super::MAX_DEPTH;
use super::entity::{entity_char, unescape_attr};
use crate::detail::{Detail, Element, Node};
use crate::error::ParseError;
use crate::event::{Event, Point, VERSION};
use crate::time::CotTime;

/// `<event>` attributes rustak models; everything else is kept verbatim.
const EVENT_ATTRS: &[&str] = &[
    "version",
    "uid",
    "type",
    "how",
    "time",
    "start",
    "stale",
    "access",
    "qos",
    "opex",
    "caveat",
    "releasableTo",
];

/// Parses one CoT message.
///
/// # Errors
///
/// Returns [`ParseError::Utf8`] for non-UTF-8 input, and otherwise whatever
/// [`parse_str`] reports.
pub fn parse(bytes: &[u8]) -> Result<Event, ParseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::Utf8)?;
    parse_str(text)
}

/// Parses one CoT message from a string.
///
/// A leading byte-order mark, an XML declaration, processing instructions and
/// any bytes before `<event` or after `</event>` are ignored.
///
/// # Errors
///
/// Returns [`ParseError::MissingEvent`] when there is no `<event>`,
/// [`ParseError::MissingPoint`] when it has no `<point lat= lon=>`,
/// [`ParseError::BadNumber`] or [`ParseError::BadTime`] for attributes that do
/// not parse, [`ParseError::TooDeep`] beyond [`MAX_DEPTH`] levels of detail,
/// and [`ParseError::Xml`] for malformed markup.
pub fn parse_str(text: &str) -> Result<Event, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = reader_for(text);
    loop {
        match reader.read_event()? {
            XmlEvent::Start(tag) if tag.name().into_inner() == "event" => {
                return read_event(&mut reader, &tag);
            }
            // A self-closing `<event/>` can carry no point, so it can never
            // be a message we are willing to relay.
            XmlEvent::Empty(tag) if tag.name().into_inner() == "event" => {
                return Err(ParseError::MissingPoint);
            }
            XmlEvent::Eof => return Err(ParseError::MissingEvent),
            _ => {}
        }
    }
}

/// Parses the children of a `<detail>` from their concatenated serialisation.
///
/// This is the inverse of [`super::write_fragment`], used for the protobuf
/// `xmlDetail` field.
///
/// # Errors
///
/// As [`parse_str`], minus the event-specific variants.
pub fn parse_fragment(text: &str) -> Result<Vec<Node>, ParseError> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let wrapped = format!("<detail>{text}</detail>");
    let mut reader = reader_for(&wrapped);
    loop {
        match reader.read_event()? {
            XmlEvent::Start(_) => return read_nodes(&mut reader, 1),
            XmlEvent::Eof => return Ok(Vec::new()),
            _ => {}
        }
    }
}

fn reader_for(text: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(text);
    let config = reader.config_mut();
    config.trim_text(false);
    config.expand_empty_elements = false;
    config.check_end_names = true;
    config.check_comments = false;
    config.allow_dangling_amp = true;
    reader
}

/// Reads an `<event>` whose start tag has already been consumed.
///
/// Missing times are tolerated rather than fatal, because TAK Server relays
/// such messages too: `start` falls back to `time` and `stale` to `start`.
fn read_event(reader: &mut Reader<&[u8]>, tag: &BytesStart<'_>) -> Result<Event, ParseError> {
    let attrs = read_attrs(tag)?;
    let attr = |name: &str| {
        attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let time = attr_time(&attrs, "time")?.unwrap_or(CotTime::EPOCH);
    let start = attr_time(&attrs, "start")?.unwrap_or(time);

    let mut point = None;
    let mut detail = Detail::new();
    loop {
        match reader.read_event()? {
            XmlEvent::Start(child) => match child.name().into_inner() {
                "point" => {
                    point = Some(read_point(&child)?);
                    skip_element(reader)?;
                }
                "detail" => detail.nodes = read_nodes(reader, 1)?,
                _ => skip_element(reader)?,
            },
            XmlEvent::Empty(child) => {
                if child.name().into_inner() == "point" {
                    point = Some(read_point(&child)?);
                }
            }
            XmlEvent::End(_) | XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(Event {
        version: attr("version").unwrap_or_else(|| VERSION.to_owned()),
        uid: attr("uid").unwrap_or_default(),
        r#type: attr("type").unwrap_or_default(),
        how: attr("how"),
        time,
        start,
        stale: attr_time(&attrs, "stale")?.unwrap_or(start),
        access: attr("access"),
        qos: attr("qos"),
        opex: attr("opex"),
        caveat: attr("caveat"),
        releasable_to: attr("releasableTo"),
        extra_attrs: attrs
            .into_iter()
            .filter(|(key, _)| !EVENT_ATTRS.contains(&key.as_str()))
            .collect(),
        point: point.ok_or(ParseError::MissingPoint)?,
        detail,
        proto_extensions: Vec::new(),
    })
}

fn read_point(tag: &BytesStart<'_>) -> Result<Point, ParseError> {
    let attrs = read_attrs(tag)?;
    let number = |name: &str, fallback: f64| -> Result<f64, ParseError> {
        match attrs.iter().find(|(key, _)| key == name) {
            Some((_, value)) => value.parse().map_err(|_| ParseError::BadNumber {
                attr: name.to_owned(),
                value: value.clone(),
            }),
            None => Ok(fallback),
        }
    };
    let required = |name: &str| -> Result<f64, ParseError> {
        if attrs.iter().any(|(key, _)| key == name) {
            number(name, 0.0)
        } else {
            Err(ParseError::MissingPoint)
        }
    };

    Ok(Point {
        lat: required("lat")?,
        lon: required("lon")?,
        hae: number("hae", Point::UNKNOWN_HAE)?,
        ce: number("ce", Point::UNKNOWN_CE)?,
        le: number("le", Point::UNKNOWN_LE)?,
    })
}

fn attr_time(attrs: &[(String, String)], name: &str) -> Result<Option<CotTime>, ParseError> {
    match attrs.iter().find(|(key, _)| key == name) {
        Some((_, value)) => CotTime::parse(value)
            .map(Some)
            .map_err(|bad| ParseError::BadTime {
                attr: name.to_owned(),
                value: bad.value,
            }),
        None => Ok(None),
    }
}

fn read_attrs(tag: &BytesStart<'_>) -> Result<Vec<(String, String)>, ParseError> {
    tag.attributes()
        .map(|attribute| {
            let attribute =
                attribute.map_err(|err| ParseError::Xml(quick_xml::Error::InvalidAttr(err)))?;
            Ok((
                attribute.key.into_inner().to_owned(),
                unescape_attr(&attribute.value),
            ))
        })
        .collect()
}

/// Reads children until the element's end tag, one level below `depth`.
fn read_nodes(reader: &mut Reader<&[u8]>, depth: usize) -> Result<Vec<Node>, ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError::TooDeep);
    }
    let mut nodes = Vec::new();
    let mut pending = Pending::default();
    loop {
        match reader.read_event()? {
            XmlEvent::Start(tag) => {
                pending.flush(&mut nodes);
                let mut element = element_of(&tag)?;
                element.children = read_nodes(reader, depth + 1)?;
                nodes.push(Node::Element(element));
            }
            XmlEvent::Empty(tag) => {
                pending.flush(&mut nodes);
                nodes.push(Node::Element(element_of(&tag)?));
            }
            XmlEvent::Text(text) => pending.text.push_str(&text.xml10_content()),
            XmlEvent::GeneralRef(reference) => {
                pending.resolved = true;
                match entity_char(&reference) {
                    Some(resolved) => pending.text.push(resolved),
                    // An entity we do not know stays exactly as written.
                    None => pending.text.push_str(&format!("&{};", &*reference)),
                }
            }
            XmlEvent::CData(data) => {
                pending.flush(&mut nodes);
                nodes.push(Node::CData(data.xml10_content().into_owned()));
            }
            XmlEvent::Comment(comment) => {
                pending.flush(&mut nodes);
                nodes.push(Node::Comment(comment.xml10_content().into_owned()));
            }
            XmlEvent::End(_) | XmlEvent::Eof => {
                pending.flush(&mut nodes);
                return Ok(nodes);
            }
            _ => {}
        }
    }
}

/// A run of character data, buffered so entity references rejoin their text.
#[derive(Default)]
struct Pending {
    text: String,
    resolved: bool,
}

impl Pending {
    /// Emits the run, dropping it when it is only insignificant whitespace.
    fn flush(&mut self, nodes: &mut Vec<Node>) {
        if !self.text.is_empty() && (self.resolved || !self.text.trim().is_empty()) {
            nodes.push(Node::Text(std::mem::take(&mut self.text)));
        }
        self.text.clear();
        self.resolved = false;
    }
}

fn element_of(tag: &BytesStart<'_>) -> Result<Element, ParseError> {
    Ok(Element {
        name: tag.name().into_inner().to_owned(),
        attrs: read_attrs(tag)?,
        children: Vec::new(),
    })
}

/// Consumes the rest of an element whose contents we do not want.
fn skip_element(reader: &mut Reader<&[u8]>) -> Result<(), ParseError> {
    let mut depth = 1_usize;
    loop {
        match reader.read_event()? {
            XmlEvent::Start(_) => depth += 1,
            XmlEvent::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            XmlEvent::Eof => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::contact::STREAMING_ENDPOINT;
    use pretty_assertions::assert_eq;

    const SA: &str = concat!(
        r#"<?xml version="1.0" encoding="UTF-8"?>"#,
        r#"<event version="2.0" uid="UID-A" type="a-f-G-U-C" how="m-g" "#,
        r#"time="2026-09-17T12:00:00.000Z" start="2026-09-17T12:00:00.000Z" "#,
        r#"stale="2026-09-17T12:01:00.000Z">"#,
        r#"<point lat="51.5074" lon="-0.1278" hae="35.0" ce="9999999.0" le="9999999.0"/>"#,
        r#"<detail><contact callsign="ALPHA" endpoint="*:-1:stcp"/></detail>"#,
        "</event>"
    );

    #[test]
    fn reads_the_envelope_point_and_detail() {
        let event = parse(SA.as_bytes()).unwrap();
        assert_eq!(event.uid, "UID-A");
        assert_eq!(event.r#type, "a-f-G-U-C");
        assert_eq!(event.how.as_deref(), Some("m-g"));
        assert_eq!(event.time, CotTime::from_millis(1_789_646_400_000));
        assert_eq!(event.stale - event.start, 60_000);
        assert_eq!(event.point.lat, 51.5074);
        assert_eq!(event.point.hae, 35.0);
        assert_eq!(event.endpoint(), Some(STREAMING_ENDPOINT));
        assert!(event.extra_attrs.is_empty());
    }

    #[test]
    fn tolerates_a_bom_single_quotes_standalone_and_trailing_bytes() {
        let text = concat!(
            "\u{feff}<?xml version='1.0' standalone='yes'?>\n",
            "<!-- leading junk -->",
            "<event uid='UID-A' type='t-x-c-t' time='2026-09-17T12:00:00Z' ",
            "start='2026-09-17T12:00:00Z' stale='2026-09-17T12:00:20Z'>",
            "<point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>",
            "</event>\n<event uid='UID-B'",
        );
        let event = parse_str(text).unwrap();
        assert_eq!(event.uid, "UID-A");
        assert_eq!(event.version, "2.0");
        assert_eq!(event.point.ce, 9_999_999.0);
        assert_eq!(event.point.lat, 0.0);
    }

    #[test]
    fn a_missing_or_self_closing_point_is_rejected() {
        let no_point = r#"<event uid="A" type="a-f"></event>"#;
        assert!(matches!(parse_str(no_point), Err(ParseError::MissingPoint)));

        let no_lat = r#"<event uid="A" type="a-f"><point lon="1"/></event>"#;
        assert!(matches!(parse_str(no_lat), Err(ParseError::MissingPoint)));

        assert!(matches!(
            parse_str(r#"<event uid="A"/>"#),
            Err(ParseError::MissingPoint)
        ));
    }

    #[test]
    fn missing_event_and_bad_numbers_and_times_are_reported() {
        assert!(matches!(parse_str(""), Err(ParseError::MissingEvent)));
        assert!(matches!(
            parse_str("<events><event/></events>"),
            Err(ParseError::MissingPoint)
        ));

        let bad_lat = r#"<event uid="A"><point lat="north" lon="1"/></event>"#;
        assert!(matches!(
            parse_str(bad_lat),
            Err(ParseError::BadNumber { ref attr, .. }) if attr == "lat"
        ));

        let bad_time = r#"<event uid="A" stale="soon"><point lat="0" lon="0"/></event>"#;
        assert!(matches!(
            parse_str(bad_time),
            Err(ParseError::BadTime { ref attr, ref value }) if attr == "stale" && value == "soon"
        ));
    }

    #[test]
    fn times_default_forward_when_attributes_are_absent() {
        let text = r#"<event uid="A" time="2026-09-17T12:00:00Z"><point lat="0" lon="0"/></event>"#;
        let event = parse_str(text).unwrap();
        assert_eq!(event.time, event.start);
        assert_eq!(event.start, event.stale);
    }

    #[test]
    fn unknown_event_attributes_are_kept_in_order() {
        let text = r#"<event uid="A" vendor="rustak" build="7"><point lat="0" lon="0"/></event>"#;
        let event = parse_str(text).unwrap();
        assert_eq!(
            event.extra_attrs,
            vec![
                ("vendor".into(), "rustak".into()),
                ("build".into(), "7".into())
            ]
        );
    }

    #[test]
    fn entities_resolve_and_unknown_ones_stay_literal() {
        let text = concat!(
            r#"<event uid="A"><point lat="0" lon="0"/><detail>"#,
            r#"<contact callsign="A &amp; B &quot;C&quot; &#66; &#x44; &nope;"/>"#,
            "<remarks>a &amp; b</remarks>",
            "</detail></event>"
        );
        let event = parse_str(text).unwrap();
        assert_eq!(event.callsign(), Some(r#"A & B "C" B D &nope;"#));
        assert_eq!(event.detail.find("remarks").unwrap().text(), "a & b");
    }

    #[test]
    fn whitespace_between_elements_is_dropped_but_inside_text_is_kept() {
        let text = concat!(
            "<event uid=\"A\"><point lat=\"0\" lon=\"0\"/><detail>\n  ",
            "<contact callsign=\"A\"/>\n  ",
            "<remarks>  spaced  </remarks>\n",
            "</detail></event>"
        );
        let event = parse_str(text).unwrap();
        assert_eq!(event.detail.nodes.len(), 2);
        assert_eq!(event.detail.find("remarks").unwrap().text(), "  spaced  ");
    }

    #[test]
    fn cdata_comments_and_nesting_survive() {
        let text = concat!(
            r#"<event uid="A"><point lat="0" lon="0"/><detail>"#,
            "<remarks><![CDATA[<b>bold</b>]]></remarks>",
            "<!-- note -->",
            r#"<__chat id="C"><chatgrp uid0="A" uid1="B"/></__chat>"#,
            "</detail></event>"
        );
        let event = parse_str(text).unwrap();
        assert_eq!(
            event.detail.find("remarks").unwrap().children,
            vec![Node::CData("<b>bold</b>".into())]
        );
        assert_eq!(event.detail.nodes[1], Node::Comment(" note ".into()));
        let chat = event.detail.find("__chat").unwrap();
        assert_eq!(chat.child("chatgrp").unwrap().get("uid1"), Some("B"));
    }

    #[test]
    fn a_self_closing_or_absent_detail_yields_an_empty_tree() {
        for text in [
            r#"<event uid="A"><point lat="0" lon="0"/><detail/></event>"#,
            r#"<event uid="A"><point lat="0" lon="0"/></event>"#,
            r#"<event uid="A"><point lat="0" lon="0"/><detail></detail></event>"#,
        ] {
            assert!(parse_str(text).unwrap().detail.is_empty(), "{text}");
        }
    }

    #[test]
    fn nesting_past_the_depth_cap_is_rejected_without_recursing_forever() {
        let deep = format!(
            r#"<event uid="A"><point lat="0" lon="0"/><detail>{}{}</detail></event>"#,
            "<a>".repeat(MAX_DEPTH + 2),
            "</a>".repeat(MAX_DEPTH + 2)
        );
        assert!(matches!(parse_str(&deep), Err(ParseError::TooDeep)));
    }

    #[test]
    fn malformed_markup_is_an_error_not_a_panic() {
        for text in [
            r#"<event uid="A"><point lat="0" lon="0"/></evt>"#,
            r#"<event uid="A"#,
            "<event uid=A><point lat='0' lon='0'/></event>",
        ] {
            assert!(parse_str(text).is_err(), "{text}");
        }
    }

    #[test]
    fn invalid_utf8_is_reported_as_such() {
        assert!(matches!(parse(&[0xff, 0xfe, 0x00]), Err(ParseError::Utf8)));
    }

    #[test]
    fn fragments_parse_without_a_wrapper() {
        let nodes =
            parse_fragment(r#"<contact callsign="ALPHA"/><__group name="Cyan" role="HQ"/>"#)
                .unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(parse_fragment("").unwrap().is_empty());
        assert!(parse_fragment("   ").unwrap().is_empty());

        let detail = Detail { nodes };
        assert_eq!(
            detail.get::<crate::detail::Group>().unwrap().role,
            "HQ".to_owned()
        );
    }

    #[test]
    fn attribute_whitespace_is_normalised_like_every_xml_parser() {
        let text = "<event uid=\"A\"><point lat=\"0\" lon=\"0\"/><detail><remarks source=\"a\tb\nc\"/></detail></event>";
        let event = parse_str(text).unwrap();
        assert_eq!(
            event.detail.find("remarks").unwrap().get("source"),
            Some("a b c")
        );
    }
}
