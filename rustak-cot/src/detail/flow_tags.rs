//! `<_flow-tags_>` — the loop-prevention marker a server stamps on relays.
//!
//! Each server adds one attribute named `TAK-Server-<server id>` whose value
//! is the time it handled the message. Seeing our own tag on an inbound
//! message means it has already been through us, so it is dropped.
//!
//! The tag is an attribute *name*, and the server id is whatever the operator
//! typed as the server's display name — `SierraSoftworks TAK`, say. Written
//! verbatim that is `<_flow-tags_ TAK-Server-SierraSoftworks TAK="…">`, which a
//! strict parser reads as an attribute with no value and refuses, and since
//! every relayed message carries the tag, every message this server relays is
//! one such a parser will not read: CloudTAK dropped each one off its socket
//! and answered `500` for a whole mission's `/cot` document. Every character
//! that may not appear in an XML name is therefore replaced with `-` before the
//! id becomes a name. An empty id disables tagging entirely, exactly as TAK
//! Server does.

use super::{Detail, Element};
use crate::time::CotTime;

/// The element flow tags live on.
pub const ELEMENT: &str = "_flow-tags_";

/// The attribute name a given server stamps.
///
/// Characters that may not appear in an XML name — a space above all — become
/// `-`, so that the name is one every parser reads. Two ids that differ only
/// in such characters share a tag, which is harmless: the tag says "this
/// server saw it", and two servers whose names differ only by punctuation are
/// not something loop prevention has to tell apart.
#[must_use]
pub fn flow_tag_name(server_id: &str) -> String {
    let safe: String = server_id
        .chars()
        .map(|c| if crate::xml::is_name_char(c) { c } else { '-' })
        .collect();

    format!("TAK-Server-{safe}")
}

/// Whether this server has already handled the message.
#[must_use]
pub fn has_flow_tag(detail: &Detail, server_id: &str) -> bool {
    if server_id.is_empty() {
        return false;
    }
    let name = flow_tag_name(server_id);
    detail
        .find_all(ELEMENT)
        .into_iter()
        .any(|element| element.get(&name).is_some())
}

/// Stamps this server's flow tag, creating `<_flow-tags_>` when absent.
///
/// A tag that is already present is refreshed rather than duplicated. An empty
/// `server_id` is a no-op.
pub fn add_flow_tag(detail: &mut Detail, server_id: &str, now: CotTime) {
    if server_id.is_empty() {
        return;
    }
    let name = flow_tag_name(server_id);
    let value = now.to_string();
    match detail.find_mut(ELEMENT) {
        Some(element) => element.set(name, value),
        None => detail.push(Element::new(ELEMENT).attr(name, value)),
    }
}

/// Removes this server's flow tag, leaving other servers' tags in place.
///
/// Used before re-injecting a stored message, so that a replay is not
/// mistaken for a loop.
pub fn remove_flow_tag(detail: &mut Detail, server_id: &str) {
    if server_id.is_empty() {
        return;
    }
    let name = flow_tag_name(server_id);
    for element in detail.elements_mut().filter(|e| e.name == ELEMENT) {
        element.remove_attr(&name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: CotTime = CotTime::from_millis(1_789_646_400_000);

    #[test]
    fn tagging_creates_the_element_then_refreshes_it() {
        let mut detail = Detail::new();
        assert!(!has_flow_tag(&detail, "rustak-1"));

        add_flow_tag(&mut detail, "rustak-1", NOW);
        assert!(has_flow_tag(&detail, "rustak-1"));
        assert_eq!(
            detail.find(ELEMENT).unwrap().get("TAK-Server-rustak-1"),
            Some("2026-09-17T12:00:00.000Z")
        );

        add_flow_tag(
            &mut detail,
            "rustak-1",
            NOW.stale_after(std::time::Duration::from_secs(1)),
        );
        assert_eq!(detail.count(ELEMENT), 1);
        assert_eq!(detail.find(ELEMENT).unwrap().attrs.len(), 1);
        assert_eq!(
            detail.find(ELEMENT).unwrap().get("TAK-Server-rustak-1"),
            Some("2026-09-17T12:00:01.000Z")
        );
    }

    #[test]
    fn a_display_name_with_a_space_still_makes_a_name_a_strict_parser_reads() {
        // The id is the operator's display name, which is free text. The
        // production incident: `SierraSoftworks TAK` became an attribute
        // named `TAK-Server-SierraSoftworks` followed by a stray `TAK="…"`.
        assert_eq!(
            flow_tag_name("SierraSoftworks TAK"),
            "TAK-Server-SierraSoftworks-TAK"
        );
        assert_eq!(
            flow_tag_name("tak.example.com"),
            "TAK-Server-tak.example.com"
        );
        assert_eq!(flow_tag_name("a/b=c\"d"), "TAK-Server-a-b-c-d");

        let mut detail = Detail::new();
        add_flow_tag(&mut detail, "SierraSoftworks TAK", NOW);
        assert!(has_flow_tag(&detail, "SierraSoftworks TAK"));
        assert!(crate::xml::is_name(&flow_tag_name("SierraSoftworks TAK")));

        let event = crate::Event::builder("a-f-G", "A")
            .point(0.0, 0.0)
            .detail(detail)
            .build();
        let written = crate::xml::write(&event);
        let reread = crate::xml::parse(&written).expect("our own output parses");
        assert!(has_flow_tag(&reread.detail, "SierraSoftworks TAK"));
    }

    #[test]
    fn a_second_server_adds_its_own_attribute() {
        let mut detail = Detail::new();
        add_flow_tag(&mut detail, "rustak-1", NOW);
        add_flow_tag(&mut detail, "takserver-a", NOW);
        assert_eq!(detail.count(ELEMENT), 1);
        assert!(has_flow_tag(&detail, "rustak-1"));
        assert!(has_flow_tag(&detail, "takserver-a"));

        remove_flow_tag(&mut detail, "rustak-1");
        assert!(!has_flow_tag(&detail, "rustak-1"));
        assert!(has_flow_tag(&detail, "takserver-a"));
    }

    #[test]
    fn an_empty_server_id_never_tags_and_never_matches() {
        let mut detail = Detail::new();
        add_flow_tag(&mut detail, "", NOW);
        assert!(detail.is_empty());
        assert!(!has_flow_tag(&detail, ""));
    }

    #[test]
    fn a_tag_on_a_later_duplicate_element_is_still_seen() {
        let mut detail = Detail::new();
        detail.push(Element::new(ELEMENT));
        detail.push(Element::new(ELEMENT).attr("TAK-Server-other", "2026-01-01T00:00:00.000Z"));
        assert!(has_flow_tag(&detail, "other"));
    }
}
