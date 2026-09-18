//! `<track>` — the reported ground speed and course.

use super::{Element, StrictDetail, TypedDetail, apply_extras, extra_attrs};

/// `<track speed= course=/>`, both as decimal doubles.
///
/// The lenient read coerces unparsable values to `0.0` so that accessors keep
/// working; [`StrictDetail::strict`] refuses them instead, which keeps a
/// malformed number inside `xmlDetail` rather than silently rewriting it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    /// Ground speed, metres per second.
    pub speed: f64,
    /// Course over ground, degrees true.
    pub course: f64,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Track {
    const KNOWN: &'static [&'static str] = &["speed", "course"];

    /// A track with both values set.
    #[must_use]
    pub fn new(speed: f64, course: f64) -> Self {
        Self {
            speed,
            course,
            extra: Vec::new(),
        }
    }
}

impl TypedDetail for Track {
    const NAME: &'static str = "track";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            speed: element
                .get("speed")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            course: element
                .get("course")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr("speed", crate::xml::format_f64(self.speed))
            .attr("course", crate::xml::format_f64(self.course));
        apply_extras(&mut element, &self.extra);
        element
    }
}

impl StrictDetail for Track {
    fn strict(element: &Element) -> Option<Self> {
        if !element.has_exactly(Self::KNOWN) || !element.children.is_empty() {
            return None;
        }
        Some(Self {
            speed: element.get("speed")?.parse().ok()?,
            course: element.get("course")?.parse().ok()?,
            extra: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_doubles() {
        let element = Element::new("track")
            .attr("speed", "1.5")
            .attr("course", "270.0");
        assert_eq!(
            Track::from_element(&element).unwrap(),
            Track::new(1.5, 270.0)
        );
        assert_eq!(Track::new(1.5, 270.0).to_element(), element);
    }

    #[test]
    fn lenient_coerces_bad_numbers_but_strict_refuses_them() {
        let element = Element::new("track")
            .attr("speed", "fast")
            .attr("course", "270");
        assert_eq!(Track::from_element(&element).unwrap().speed, 0.0);
        assert!(Track::strict(&element).is_none());
    }

    #[test]
    fn strict_needs_exactly_speed_and_course() {
        let extra = Element::new("track")
            .attr("speed", "1")
            .attr("course", "2")
            .attr("slope", "3");
        assert!(Track::strict(&extra).is_none());
        assert!(Track::strict(&Element::new("track").attr("speed", "1")).is_none());
    }
}
