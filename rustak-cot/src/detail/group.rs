//! `<__group>` — the channel (team colour) and role a device reports.

use super::{Element, StrictDetail, TypedDetail, apply_extras, extra_attrs};

/// `<__group name= role=/>`.
///
/// `name` is the team colour ATAK shows (`Cyan`, `Dark Green`, …) and `role`
/// the team role (`Team Member`, `HQ`, …). The server caches both from every
/// situational-awareness message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    /// Team name / colour.
    pub name: String,
    /// Team role.
    pub role: String,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl Group {
    const KNOWN: &'static [&'static str] = &["name", "role"];

    /// A group with both fields set.
    #[must_use]
    pub fn new(name: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: role.into(),
            extra: Vec::new(),
        }
    }
}

impl TypedDetail for Group {
    const NAME: &'static str = "__group";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            name: element.get("name").unwrap_or_default().to_owned(),
            role: element.get("role").unwrap_or_default().to_owned(),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr("name", self.name.clone())
            .attr("role", self.role.clone());
        apply_extras(&mut element, &self.extra);
        element
    }
}

impl StrictDetail for Group {
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
    fn reads_and_writes_the_pair() {
        let element = Element::new("__group")
            .attr("name", "Cyan")
            .attr("role", "Team Member");
        let group = Group::from_element(&element).unwrap();
        assert_eq!(group, Group::new("Cyan", "Team Member"));
        assert_eq!(group.to_element(), element);
        assert!(Group::strict(&element).is_some());
    }

    #[test]
    fn strict_needs_exactly_name_and_role() {
        assert!(Group::strict(&Element::new("__group").attr("name", "Cyan")).is_none());
        let extra = Element::new("__group")
            .attr("name", "Cyan")
            .attr("role", "HQ")
            .attr("colour", "#00FFFF");
        assert!(Group::strict(&extra).is_none());
        assert_eq!(
            Group::from_element(&extra).unwrap().extra,
            vec![("colour".into(), "#00FFFF".into())]
        );
    }
}
