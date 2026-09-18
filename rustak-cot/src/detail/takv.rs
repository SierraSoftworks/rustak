//! `<takv>` — the reporting device's hardware and software identity.

use super::{Element, StrictDetail, TypedDetail, apply_extras, extra_attrs};

/// `<takv device= platform= os= version=/>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Takv {
    /// Hardware model, e.g. `Google Pixel 8`.
    pub device: String,
    /// Application name, e.g. `ATAK-CIV`.
    pub platform: String,
    /// Operating-system version, e.g. `34`.
    pub os: String,
    /// Application version.
    pub version: String,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Takv {
    const KNOWN: &'static [&'static str] = &["device", "platform", "os", "version"];

    /// The `platform:version` string the server caches per subscription.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("{}:{}", self.platform, self.version)
    }
}

impl TypedDetail for Takv {
    const NAME: &'static str = "takv";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            device: element.get("device").unwrap_or_default().to_owned(),
            platform: element.get("platform").unwrap_or_default().to_owned(),
            os: element.get("os").unwrap_or_default().to_owned(),
            version: element.get("version").unwrap_or_default().to_owned(),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr("device", self.device.clone())
            .attr("platform", self.platform.clone())
            .attr("os", self.os.clone())
            .attr("version", self.version.clone());
        apply_extras(&mut element, &self.extra);
        element
    }
}

impl StrictDetail for Takv {
    fn strict(element: &Element) -> Option<Self> {
        if !element.has_exactly(Self::KNOWN) || !element.children.is_empty() {
            return None;
        }
        Self::from_element(element)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element() -> Element {
        Element::new("takv")
            .attr("device", "Google Pixel 8")
            .attr("platform", "ATAK-CIV")
            .attr("os", "34")
            .attr("version", "5.4.0")
    }

    #[test]
    fn reads_writes_and_summarises() {
        let takv = Takv::from_element(&element()).unwrap();
        assert_eq!(takv.summary(), "ATAK-CIV:5.4.0");
        assert_eq!(takv.to_element(), element());
        assert!(Takv::strict(&element()).is_some());
    }

    #[test]
    fn strict_needs_all_four_attributes_and_no_more() {
        let mut short = element();
        short.remove_attr("os");
        assert!(Takv::strict(&short).is_none());
        assert!(Takv::strict(&element().attr("build", "42")).is_none());
    }
}
