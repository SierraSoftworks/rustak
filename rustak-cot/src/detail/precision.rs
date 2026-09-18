//! `<precisionlocation>` — where the position and altitude came from.

use super::{Element, StrictDetail, TypedDetail, apply_extras, extra_attrs};

/// `<precisionlocation geopointsrc= altsrc=/>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrecisionLocation {
    /// Position source, e.g. `GPS`, `USER`, `DTED0`.
    pub geopointsrc: String,
    /// Altitude source, e.g. `GPS`, `DTED0`.
    pub altsrc: String,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl PrecisionLocation {
    const KNOWN: &'static [&'static str] = &["geopointsrc", "altsrc"];

    /// A precision location with both sources set.
    #[must_use]
    pub fn new(geopointsrc: impl Into<String>, altsrc: impl Into<String>) -> Self {
        Self {
            geopointsrc: geopointsrc.into(),
            altsrc: altsrc.into(),
            extra: Vec::new(),
        }
    }
}

impl TypedDetail for PrecisionLocation {
    const NAME: &'static str = "precisionlocation";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            geopointsrc: element.get("geopointsrc").unwrap_or_default().to_owned(),
            altsrc: element.get("altsrc").unwrap_or_default().to_owned(),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr("geopointsrc", self.geopointsrc.clone())
            .attr("altsrc", self.altsrc.clone());
        apply_extras(&mut element, &self.extra);
        element
    }
}

impl StrictDetail for PrecisionLocation {
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

    #[test]
    fn reads_and_writes_both_sources() {
        let element = Element::new("precisionlocation")
            .attr("geopointsrc", "GPS")
            .attr("altsrc", "DTED0");
        assert_eq!(
            PrecisionLocation::from_element(&element).unwrap(),
            PrecisionLocation::new("GPS", "DTED0")
        );
        assert_eq!(PrecisionLocation::new("GPS", "DTED0").to_element(), element);
        assert!(PrecisionLocation::strict(&element).is_some());
    }

    #[test]
    fn strict_refuses_the_single_attribute_form_atak_also_emits() {
        let element = Element::new("precisionlocation").attr("altsrc", "GPS");
        assert!(PrecisionLocation::strict(&element).is_none());
        assert_eq!(
            PrecisionLocation::from_element(&element).unwrap().altsrc,
            "GPS"
        );
    }
}
