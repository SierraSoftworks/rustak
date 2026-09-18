//! `<TakControl>` — the TAK Protocol v1 negotiation payload.
//!
//! All three negotiation messages carry the same element with different
//! children:
//!
//! * server announcement (`t-x-takp-v`): `<TakProtocolSupport version="1"/>`
//!   plus `<TakServerVersionInfo serverVersion= apiVersion=/>`;
//! * client request (`t-x-takp-q`): `<TakRequest version="1"/>`;
//! * server response (`t-x-takp-r`): `<TakResponse status="true"/>`.

use super::{Element, TypedDetail};

/// The parsed contents of a `<TakControl>` element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TakControl {
    /// Protocol versions the announcer supports, in document order.
    pub supported: Vec<u32>,
    /// The announcing server's version string, shown by CloudTAK.
    pub server_version: Option<String>,
    /// The announcing server's Marti API version.
    pub api_version: Option<u32>,
    /// The version a client is asking to switch to.
    pub request: Option<u32>,
    /// Whether the server accepted the request.
    pub response: Option<bool>,
}

impl TakControl {
    /// A server announcement for one protocol version.
    #[must_use]
    pub fn announce(version: u32, server_version: impl Into<String>, api_version: u32) -> Self {
        Self {
            supported: vec![version],
            server_version: Some(server_version.into()),
            api_version: Some(api_version),
            ..Self::default()
        }
    }

    /// A client request for one protocol version.
    #[must_use]
    pub fn request(version: u32) -> Self {
        Self {
            request: Some(version),
            ..Self::default()
        }
    }

    /// A server response.
    #[must_use]
    pub fn response(accepted: bool) -> Self {
        Self {
            response: Some(accepted),
            ..Self::default()
        }
    }

    /// Whether the announcement offers this protocol version.
    #[must_use]
    pub fn supports(&self, version: u32) -> bool {
        self.supported.contains(&version)
    }
}

impl TypedDetail for TakControl {
    const NAME: &'static str = "TakControl";

    fn from_element(element: &Element) -> Option<Self> {
        let version_info = element.child("TakServerVersionInfo");
        Some(Self {
            supported: element
                .elements()
                .filter(|child| child.name == "TakProtocolSupport")
                .filter_map(|child| child.get("version")?.parse().ok())
                .collect(),
            server_version: version_info
                .and_then(|info| info.get("serverVersion"))
                .map(ToOwned::to_owned),
            api_version: version_info
                .and_then(|info| info.get("apiVersion"))
                .and_then(|value| value.parse().ok()),
            request: element
                .child("TakRequest")
                .and_then(|child| child.get("version"))
                .and_then(|value| value.parse().ok()),
            response: element
                .child("TakResponse")
                .and_then(|child| child.get("status"))
                .map(|value| value.eq_ignore_ascii_case("true")),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME);
        for version in &self.supported {
            element.push(Element::new("TakProtocolSupport").attr("version", version.to_string()));
        }
        if self.server_version.is_some() || self.api_version.is_some() {
            element.push(
                Element::new("TakServerVersionInfo")
                    .attr_opt("serverVersion", self.server_version.clone())
                    .attr_opt("apiVersion", self.api_version.map(|v| v.to_string())),
            );
        }
        if let Some(version) = self.request {
            element.push(Element::new("TakRequest").attr("version", version.to_string()));
        }
        if let Some(status) = self.response {
            element.push(Element::new("TakResponse").attr("status", status.to_string()));
        }
        element
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_round_trips_with_both_children() {
        let control = TakControl::announce(1, "rustak-0.1.0", 3);
        let element = control.to_element();
        assert_eq!(element.elements().count(), 2);
        assert_eq!(
            element.child("TakProtocolSupport").unwrap().get("version"),
            Some("1")
        );
        assert_eq!(
            element
                .child("TakServerVersionInfo")
                .unwrap()
                .get("apiVersion"),
            Some("3")
        );
        assert_eq!(TakControl::from_element(&element), Some(control));
    }

    #[test]
    fn request_and_response_round_trip() {
        let request = TakControl::request(1);
        assert_eq!(
            TakControl::from_element(&request.to_element()),
            Some(request.clone())
        );
        assert_eq!(request.request, Some(1));

        for accepted in [true, false] {
            let response = TakControl::response(accepted);
            let element = response.to_element();
            assert_eq!(
                element.child("TakResponse").unwrap().get("status"),
                Some(if accepted { "true" } else { "false" })
            );
            assert_eq!(TakControl::from_element(&element), Some(response));
        }
    }

    #[test]
    fn several_supported_versions_keep_their_order() {
        let element = Element::new("TakControl")
            .with(Element::new("TakProtocolSupport").attr("version", "1"))
            .with(Element::new("TakProtocolSupport").attr("version", "2"))
            .with(Element::new("TakProtocolSupport").attr("version", "oops"));
        let control = TakControl::from_element(&element).unwrap();
        assert_eq!(control.supported, vec![1, 2]);
        assert!(control.supports(2));
        assert!(!control.supports(3));
    }

    #[test]
    fn status_is_read_case_insensitively() {
        let element =
            Element::new("TakControl").with(Element::new("TakResponse").attr("status", "TRUE"));
        assert_eq!(
            TakControl::from_element(&element).unwrap().response,
            Some(true)
        );
    }

    #[test]
    fn an_empty_control_renders_nothing_but_the_wrapper() {
        let element = TakControl::default().to_element();
        assert!(element.children.is_empty());
        assert_eq!(element.name, "TakControl");
    }
}
