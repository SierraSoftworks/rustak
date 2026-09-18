//! `<_flow-tags_>` — the loop-prevention marker a server stamps on relays.
//!
//! Each server adds one attribute named `TAK-Server-<server id>` whose value
//! is the time it handled the message. Seeing our own tag on an inbound
//! message means it has already been through us, so it is dropped.
//!
//! The server id must therefore be usable as an XML attribute name; an empty
//! id disables tagging entirely, exactly as TAK Server does.

use super::{Detail, Element};
use crate::time::CotTime;

/// The element flow tags live on.
pub const ELEMENT: &str = "_flow-tags_";

/// The attribute name a given server stamps.
#[must_use]
pub fn flow_tag_name(server_id: &str) -> String {
    format!("TAK-Server-{server_id}")
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
