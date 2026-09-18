//! `<link>` — a relationship to another CoT object.
//!
//! Links carry the peer uid in a `t-x-d-d` disconnect, the sender identity on
//! a GeoChat message, and parent/child relationships between map items.

use super::{Detail, Element, TypedDetail, apply_extras, extra_attrs};

/// The peer-to-peer relation ATAK uses for disconnects and chat senders.
pub const RELATION_P_P: &str = "p-p";

/// `<link relation= uid= type= parent_callsign= production_time=/>`.
///
/// Every attribute is optional: a group-change notice carries a `<link>` with
/// nothing but `relation="p-p"`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// How the linked object relates to this event, e.g. `p-p`.
    pub relation: Option<String>,
    /// The linked object's uid.
    pub uid: Option<String>,
    /// The linked object's CoT type.
    pub r#type: Option<String>,
    /// Callsign of the parent that produced the linked object.
    pub parent_callsign: Option<String>,
    /// When the linked object was produced.
    pub production_time: Option<String>,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Link {
    const KNOWN: &'static [&'static str] = &[
        "relation",
        "uid",
        "type",
        "parent_callsign",
        "production_time",
    ];

    /// A `p-p` link naming a peer and its CoT type.
    #[must_use]
    pub fn peer(uid: impl Into<String>, r#type: impl Into<String>) -> Self {
        Self {
            relation: Some(RELATION_P_P.to_owned()),
            uid: Some(uid.into()),
            r#type: Some(r#type.into()),
            ..Self::default()
        }
    }

    /// A bare `<link relation="p-p"/>`, as sent with a group-change notice.
    #[must_use]
    pub fn bare_peer() -> Self {
        Self {
            relation: Some(RELATION_P_P.to_owned()),
            ..Self::default()
        }
    }
}

impl TypedDetail for Link {
    const NAME: &'static str = "link";

    fn from_element(element: &Element) -> Option<Self> {
        let attr = |name: &str| element.get(name).map(ToOwned::to_owned);
        Some(Self {
            relation: attr("relation"),
            uid: attr("uid"),
            r#type: attr("type"),
            parent_callsign: attr("parent_callsign"),
            production_time: attr("production_time"),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr_opt("uid", self.uid.clone())
            .attr_opt("type", self.r#type.clone())
            .attr_opt("parent_callsign", self.parent_callsign.clone())
            .attr_opt("relation", self.relation.clone())
            .attr_opt("production_time", self.production_time.clone());
        apply_extras(&mut element, &self.extra);
        element
    }
}

/// Every `<link>` child of a detail, in document order.
///
/// A `t-x-d-d` may name several objects at once, so this is the accessor to
/// use rather than [`Detail::get`].
#[must_use]
pub fn links(detail: &Detail) -> Vec<Link> {
    detail
        .find_all(Link::NAME)
        .into_iter()
        .filter_map(Link::from_element)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_link_in_order() {
        let mut detail = Detail::new();
        detail.push(Link::peer("UID-1", "a-f-G-U-C").to_element());
        detail.push(Element::new("contact").attr("callsign", "A"));
        detail.push(Link::peer("UID-2", "a-h-G").to_element());

        let found = links(&detail);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].uid.as_deref(), Some("UID-1"));
        assert_eq!(found[1].r#type.as_deref(), Some("a-h-G"));
        assert!(found.iter().all(|l| l.relation.as_deref() == Some("p-p")));
    }

    #[test]
    fn a_bare_peer_link_carries_only_the_relation() {
        let element = Link::bare_peer().to_element();
        assert_eq!(element.attrs, vec![("relation".into(), "p-p".into())]);
        assert_eq!(Link::from_element(&element).unwrap(), Link::bare_peer());
    }

    #[test]
    fn unmodelled_attributes_survive_a_round_trip() {
        let element = Element::new("link")
            .attr("uid", "UID")
            .attr("relation", "p-p")
            .attr("remarks", "seen");
        let link = Link::from_element(&element).unwrap();
        assert_eq!(link.extra, vec![("remarks".into(), "seen".into())]);
        assert_eq!(link.to_element().get("remarks"), Some("seen"));
    }
}
