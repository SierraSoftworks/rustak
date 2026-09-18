//! The `<detail>` tree and the typed views over it.
//!
//! CoT's `<detail>` is open-ended: every client bolts its own elements on, and
//! a server that re-serialises a message must hand the next hop everything it
//! did not understand. So the model here is an **ordered node tree** rather
//! than a struct of known fields.
//!
//! Fidelity is semantic, not byte-for-byte: element names, attribute names,
//! attribute order, values, text and CDATA all survive a parse/write cycle,
//! but insignificant whitespace between elements does not.
//!
//! Typed details ([`TypedDetail`]) are *views*: they read a known element out
//! of the tree and write one back, leaving everything else alone.

pub mod chat;
pub mod contact;
pub mod fileshare;
pub mod flow_tags;
pub mod group;
pub mod link;
pub mod marti;
pub mod mission;
pub mod precision;
pub mod status;
pub mod takcontrol;
pub mod takv;
pub mod track;

pub use chat::{Chat, Remarks};
pub use contact::Contact;
pub use fileshare::{AckRequest, AckResponse, FileShare};
pub use group::Group;
pub use link::Link;
pub use marti::{Dest, DestKind};
pub use mission::{MissionChange, MissionDetail, MissionNotice};
pub use precision::PrecisionLocation;
pub use status::Status;
pub use takcontrol::TakControl;
pub use takv::Takv;
pub use track::Track;

/// One node in a `<detail>` tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A child element.
    Element(Element),
    /// Character data, already unescaped.
    Text(String),
    /// A `<![CDATA[…]]>` section, verbatim.
    CData(String),
    /// An XML comment, without its `<!--`/`-->` delimiters.
    Comment(String),
}

impl From<Element> for Node {
    fn from(value: Element) -> Self {
        Self::Element(value)
    }
}

/// An XML element inside `<detail>`.
///
/// Attribute order is preserved because ATAK's own parsers are order-tolerant
/// but our golden tests are not: keeping the order makes a relayed message
/// byte-identical to the one that arrived.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    /// The element name, including any namespace prefix, verbatim.
    pub name: String,
    /// Attributes in document order, values unescaped.
    pub attrs: Vec<(String, String)>,
    /// Child nodes in document order.
    pub children: Vec<Node>,
}

impl Element {
    /// An empty element with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Builder-style attribute setter.
    #[must_use]
    pub fn attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.set(key, value);
        self
    }

    /// Builder-style attribute setter that skips [`None`].
    #[must_use]
    pub fn attr_opt(self, key: impl Into<String>, value: Option<impl Into<String>>) -> Self {
        match value {
            Some(value) => self.attr(key, value),
            None => self,
        }
    }

    /// The first value of an attribute, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Sets an attribute, replacing the first existing one of that name and
    /// otherwise appending at the end.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        match self.attrs.iter_mut().find(|(name, _)| *name == key) {
            Some(slot) => slot.1 = value.into(),
            None => self.attrs.push((key, value.into())),
        }
    }

    /// Removes every attribute of that name, returning the first value.
    pub fn remove_attr(&mut self, key: &str) -> Option<String> {
        let mut removed = None;
        self.attrs.retain(|(name, value)| {
            if name == key {
                if removed.is_none() {
                    removed = Some(value.clone());
                }
                false
            } else {
                true
            }
        });
        removed
    }

    /// The child elements, in document order.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// The first child element with this name.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|element| element.name == name)
    }

    /// Appends a child node.
    pub fn push(&mut self, node: impl Into<Node>) {
        self.children.push(node.into());
    }

    /// Builder-style child append.
    #[must_use]
    pub fn with(mut self, node: impl Into<Node>) -> Self {
        self.push(node);
        self
    }

    /// The concatenated character data directly inside this element,
    /// including CDATA sections but not nested elements.
    #[must_use]
    pub fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|node| match node {
                Node::Text(text) | Node::CData(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Whether this element carries no attributes and no children.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attrs.is_empty() && self.children.is_empty()
    }

    /// Whether the attribute names are exactly `expected`, in any order.
    ///
    /// The protobuf encoding may only replace an element with a typed
    /// sub-message when its attribute set matches exactly, so this is the
    /// shared precondition of every [`StrictDetail`] implementation.
    #[must_use]
    pub fn has_exactly(&self, expected: &[&str]) -> bool {
        self.attrs.len() == expected.len()
            && self
                .attrs
                .iter()
                .all(|(name, _)| expected.contains(&name.as_str()))
    }
}

/// The `<detail>` element of an event: an ordered list of nodes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detail {
    /// Child nodes in document order.
    pub nodes: Vec<Node>,
}

impl Detail {
    /// An empty detail, which is omitted entirely when the event is written.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether there is nothing to serialise.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The child elements, in document order.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.nodes.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// The child elements, mutably, in document order.
    pub fn elements_mut(&mut self) -> impl Iterator<Item = &mut Element> {
        self.nodes.iter_mut().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// The first top-level element with this name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Element> {
        self.elements().find(|element| element.name == name)
    }

    /// Every top-level element with this name, in document order.
    #[must_use]
    pub fn find_all(&self, name: &str) -> Vec<&Element> {
        self.elements()
            .filter(|element| element.name == name)
            .collect()
    }

    /// The first top-level element with this name, mutably.
    pub fn find_mut(&mut self, name: &str) -> Option<&mut Element> {
        self.elements_mut().find(|element| element.name == name)
    }

    /// How many top-level elements carry this name.
    #[must_use]
    pub fn count(&self, name: &str) -> usize {
        self.elements()
            .filter(|element| element.name == name)
            .count()
    }

    /// Removes and returns every top-level element with this name.
    pub fn remove_all(&mut self, name: &str) -> Vec<Element> {
        let mut removed = Vec::new();
        self.nodes.retain(|node| match node {
            Node::Element(element) if element.name == name => {
                removed.push(element.clone());
                false
            }
            _ => true,
        });
        removed
    }

    /// Appends an element.
    pub fn push(&mut self, element: Element) {
        self.nodes.push(Node::Element(element));
    }

    /// Appends any node.
    pub fn push_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    /// Reads a typed view of the first element of its name.
    #[must_use]
    pub fn get<T: TypedDetail>(&self) -> Option<T> {
        self.find(T::NAME).and_then(T::from_element)
    }

    /// Writes a typed view back, replacing the first element of its name and
    /// otherwise appending at the end.
    pub fn set<T: TypedDetail>(&mut self, value: &T) {
        let element = value.to_element();
        match self.find_mut(T::NAME) {
            Some(slot) => *slot = element,
            None => self.push(element),
        }
    }
}

impl FromIterator<Element> for Detail {
    fn from_iter<I: IntoIterator<Item = Element>>(iter: I) -> Self {
        Self {
            nodes: iter.into_iter().map(Node::Element).collect(),
        }
    }
}

/// A named `<detail>` child with a known shape.
///
/// [`from_element`](TypedDetail::from_element) is deliberately *lenient*: it
/// reads what it recognises and ignores the rest, because accessors should
/// work on messages from clients that add their own attributes.
pub trait TypedDetail: Sized {
    /// The element name this view binds to.
    const NAME: &'static str;

    /// Reads the view from an element, leniently.
    fn from_element(element: &Element) -> Option<Self>;

    /// Renders the view back to an element.
    fn to_element(&self) -> Element;
}

/// A typed detail that the TAK Protocol carries as its own sub-message.
///
/// [`strict`](StrictDetail::strict) applies the spec rule: the element may
/// only be replaced by a typed sub-message when it has **exactly** the
/// expected attribute set, no children, and every numeric attribute parses.
/// Anything else stays in `xmlDetail`, so that a decoder on the far side sees
/// what the sender actually wrote.
pub trait StrictDetail: TypedDetail {
    /// Reads the view only when the element matches the protobuf shape exactly.
    fn strict(element: &Element) -> Option<Self>;
}

/// The attributes of `element` whose names are not in `known`, in document order.
///
/// Typed views keep these so that reading and re-writing a `<detail>` child
/// never silently drops a vendor attribute.
pub(crate) fn extra_attrs(element: &Element, known: &[&str]) -> Vec<(String, String)> {
    element
        .attrs
        .iter()
        .filter(|(name, _)| !known.contains(&name.as_str()))
        .cloned()
        .collect()
}

/// Appends previously captured extra attributes to a rendered element.
pub(crate) fn apply_extras(element: &mut Element, extras: &[(String, String)]) {
    for (name, value) in extras {
        element.set(name.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Detail {
        let mut detail = Detail::new();
        detail.push(Element::new("contact").attr("callsign", "ALPHA"));
        detail.push_node(Node::Comment(" hand written ".into()));
        detail.push(Element::new("remarks").with(Node::Text("hello".into())));
        detail.push(Element::new("contact").attr("callsign", "BRAVO"));
        detail
    }

    #[test]
    fn attribute_order_survives_set_and_remove() {
        let mut element = Element::new("takv")
            .attr("device", "PIXEL")
            .attr("platform", "ATAK")
            .attr("os", "34");
        element.set("device", "PIXEL 8");
        assert_eq!(
            element.attrs,
            vec![
                ("device".into(), "PIXEL 8".into()),
                ("platform".into(), "ATAK".into()),
                ("os".into(), "34".into()),
            ]
        );
        assert_eq!(element.remove_attr("platform").as_deref(), Some("ATAK"));
        assert_eq!(element.get("platform"), None);
        assert_eq!(element.attrs.len(), 2);
    }

    #[test]
    fn remove_attr_strips_every_duplicate_but_reports_the_first() {
        let mut element = Element::new("x");
        element.attrs.push(("a".into(), "1".into()));
        element.attrs.push(("a".into(), "2".into()));
        assert_eq!(element.remove_attr("a").as_deref(), Some("1"));
        assert!(element.attrs.is_empty());
    }

    #[test]
    fn find_all_and_remove_all_see_every_duplicate() {
        let mut detail = sample();
        assert_eq!(detail.count("contact"), 2);
        assert_eq!(
            detail.find("contact").unwrap().get("callsign"),
            Some("ALPHA")
        );
        assert_eq!(detail.find_all("contact").len(), 2);

        let removed = detail.remove_all("contact");
        assert_eq!(removed.len(), 2);
        assert_eq!(detail.count("contact"), 0);
        // The comment and the remarks element are untouched.
        assert_eq!(detail.nodes.len(), 2);
    }

    #[test]
    fn text_concatenates_character_data_but_not_children() {
        let element = Element::new("remarks")
            .with(Node::Text("hello ".into()))
            .with(Element::new("b").with(Node::Text("IGNORED".into())))
            .with(Node::CData("world".into()));
        assert_eq!(element.text(), "hello world");
    }

    #[test]
    fn has_exactly_is_order_insensitive_and_size_sensitive() {
        let element = Element::new("contact")
            .attr("endpoint", "*:-1:stcp")
            .attr("callsign", "ALPHA");
        assert!(element.has_exactly(&["callsign", "endpoint"]));
        assert!(!element.has_exactly(&["callsign"]));
        assert!(!element.has_exactly(&["callsign", "endpoint", "phone"]));
    }

    #[test]
    fn empty_detail_round_trips_through_from_iter() {
        let detail: Detail = std::iter::empty::<Element>().collect();
        assert!(detail.is_empty());
        let detail: Detail = [Element::new("a"), Element::new("b")].into_iter().collect();
        assert_eq!(detail.elements().count(), 2);
    }

    #[test]
    fn child_lookup_only_walks_one_level() {
        let element =
            Element::new("__chat").with(Element::new("chatgrp").with(Element::new("deep")));
        assert!(element.child("chatgrp").is_some());
        assert!(element.child("deep").is_none());
        assert!(element.child("chatgrp").unwrap().child("deep").is_some());
    }
}
