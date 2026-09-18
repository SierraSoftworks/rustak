//! `<status>` — the reporting device's battery level.

use super::{Element, StrictDetail, TypedDetail, apply_extras, extra_attrs};

/// `<status battery=/>`, a whole percentage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// Battery charge, 0-100.
    pub battery: u32,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Status {
    const KNOWN: &'static [&'static str] = &["battery"];

    /// A status with just a battery level.
    #[must_use]
    pub fn new(battery: u32) -> Self {
        Self {
            battery,
            extra: Vec::new(),
        }
    }
}

impl TypedDetail for Status {
    const NAME: &'static str = "status";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            battery: element
                .get("battery")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME).attr("battery", self.battery.to_string());
        apply_extras(&mut element, &self.extra);
        element
    }
}

impl StrictDetail for Status {
    fn strict(element: &Element) -> Option<Self> {
        if !element.has_exactly(Self::KNOWN) || !element.children.is_empty() {
            return None;
        }
        Some(Self {
            battery: element.get("battery")?.parse().ok()?,
            extra: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_the_battery_level() {
        let element = Element::new("status").attr("battery", "87");
        assert_eq!(Status::from_element(&element).unwrap(), Status::new(87));
        assert_eq!(Status::new(87).to_element(), element);
        assert!(Status::strict(&element).is_some());
    }

    #[test]
    fn lenient_zeroes_a_bad_battery_but_strict_refuses_it() {
        let element = Element::new("status").attr("battery", "87.5");
        assert_eq!(Status::from_element(&element).unwrap().battery, 0);
        assert!(Status::strict(&element).is_none());
        assert!(Status::strict(&Element::new("status").attr("battery", "-1")).is_none());
    }

    #[test]
    fn strict_refuses_the_extra_attributes_atak_sometimes_adds() {
        let element = Element::new("status")
            .attr("battery", "87")
            .attr("readiness", "true");
        assert!(Status::strict(&element).is_none());
        assert_eq!(Status::from_element(&element).unwrap().battery, 87);
    }
}
