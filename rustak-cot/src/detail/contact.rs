//! `<contact>` — the callsign and reply endpoint of the sending device.

use super::{Element, StrictDetail, TypedDetail, apply_extras, extra_attrs};

/// The streaming reply sentinel ATAK writes on every message sent to a server.
///
/// It is emitted verbatim, with no port or protocol suffix appended.
pub const STREAMING_ENDPOINT: &str = "*:-1:stcp";

/// `<contact callsign= endpoint=/>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    /// The device's display name. Empty when the element omits it.
    pub callsign: String,
    /// Where to reply, e.g. [`STREAMING_ENDPOINT`].
    pub endpoint: Option<String>,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Contact {
    /// The attribute names the protobuf `Contact` sub-message can carry.
    const KNOWN: &'static [&'static str] = &["callsign", "endpoint"];

    /// A contact with just a callsign.
    #[must_use]
    pub fn new(callsign: impl Into<String>) -> Self {
        Self {
            callsign: callsign.into(),
            ..Self::default()
        }
    }

    /// Builder-style endpoint setter.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
}

impl TypedDetail for Contact {
    const NAME: &'static str = "contact";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            callsign: element.get("callsign").unwrap_or_default().to_owned(),
            endpoint: element.get("endpoint").map(ToOwned::to_owned),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr("callsign", self.callsign.clone())
            .attr_opt("endpoint", self.endpoint.clone());
        apply_extras(&mut element, &self.extra);
        element
    }
}

impl StrictDetail for Contact {
    fn strict(element: &Element) -> Option<Self> {
        let matches_shape = element.has_exactly(&["callsign"]) || element.has_exactly(Self::KNOWN);
        if !matches_shape || !element.children.is_empty() {
            return None;
        }
        Self::from_element(element)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::Node;

    #[test]
    fn lenient_read_keeps_unmodelled_attributes() {
        let element = Element::new("contact")
            .attr("callsign", "ALPHA")
            .attr("endpoint", STREAMING_ENDPOINT)
            .attr("phone", "555");
        let contact = Contact::from_element(&element).unwrap();
        assert_eq!(contact.callsign, "ALPHA");
        assert_eq!(contact.endpoint.as_deref(), Some(STREAMING_ENDPOINT));
        assert_eq!(contact.extra, vec![("phone".into(), "555".into())]);
        assert_eq!(contact.to_element(), element);
    }

    #[test]
    fn strict_accepts_only_the_two_protobuf_shapes() {
        assert!(Contact::strict(&Element::new("contact").attr("callsign", "A")).is_some());
        assert!(
            Contact::strict(
                &Element::new("contact")
                    .attr("callsign", "A")
                    .attr("endpoint", STREAMING_ENDPOINT)
            )
            .is_some()
        );
    }

    #[test]
    fn strict_rejects_extra_attributes_missing_callsign_and_children() {
        let extra = Element::new("contact")
            .attr("callsign", "A")
            .attr("phone", "555");
        assert!(Contact::strict(&extra).is_none());

        let endpoint_only = Element::new("contact").attr("endpoint", STREAMING_ENDPOINT);
        assert!(Contact::strict(&endpoint_only).is_none());

        let with_child = Element::new("contact")
            .attr("callsign", "A")
            .with(Node::Text("x".into()));
        assert!(Contact::strict(&with_child).is_none());
    }
}
