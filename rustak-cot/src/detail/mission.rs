//! `<mission>` — the mission notification payload (skeleton).
//!
//! Mission notices (`t-x-m-…`) carry a `<mission>` element whose attributes
//! name the mission and whose optional `<MissionChanges>` child lists what
//! changed. Unusually for CoT, every field of a `MissionChange` is a child
//! *element* rather than an attribute.
//!
//! M1 ships the parser and builder; M4 fills in the change payloads once the
//! mission service exists. Unmodelled children (`details`, `contentResource`,
//! `tempLogEntry`, `content`) are kept as raw [`Element`]s so nothing is lost
//! in the meantime.

use super::{Element, Node, TypedDetail, apply_extras, extra_attrs};

/// The `mission/@type` values TAK Server emits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MissionNotice {
    /// Mission content, log, keyword, metadata or layer changed.
    #[default]
    Change,
    /// A client was invited, or had its role changed (`t-x-m-r` also uses this).
    Invite,
    /// A mission was created.
    Create,
    /// A mission was deleted.
    Delete,
    /// Anything else, verbatim.
    Other(String),
}

impl MissionNotice {
    /// The wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Change => "CHANGE",
            Self::Invite => "INVITE",
            Self::Create => "CREATE",
            Self::Delete => "DELETE",
            Self::Other(value) => value,
        }
    }
}

impl From<&str> for MissionNotice {
    fn from(value: &str) -> Self {
        match value {
            "CHANGE" => Self::Change,
            "INVITE" => Self::Invite,
            "CREATE" => Self::Create,
            "DELETE" => Self::Delete,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// One `<MissionChange>`: every field is a child element, not an attribute.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MissionChange {
    /// `CREATE_MISSION`, `ADD_CONTENT`, `REMOVE_CONTENT`, …
    pub r#type: Option<String>,
    /// Whether the change arrived over federation.
    pub is_federated_change: Option<bool>,
    /// Mission name.
    pub mission_name: Option<String>,
    /// Mission GUID.
    pub mission_guid: Option<String>,
    /// When the change happened.
    pub timestamp: Option<String>,
    /// Who made the change.
    pub creator_uid: Option<String>,
    /// The uid of the content that changed.
    pub content_uid: Option<String>,
    /// Child elements we do not model yet, in document order.
    pub other: Vec<Element>,
}

impl MissionChange {
    /// Child elements this struct models by name.
    const KNOWN: &'static [&'static str] = &[
        "type",
        "isFederatedChange",
        "missionName",
        "missionGuid",
        "timestamp",
        "creatorUid",
        "contentUid",
    ];

    /// Reads a `<MissionChange>` element.
    #[must_use]
    pub fn from_element(element: &Element) -> Self {
        let text = |name: &str| element.child(name).map(Element::text);
        Self {
            r#type: text("type"),
            is_federated_change: text("isFederatedChange")
                .map(|value| value.eq_ignore_ascii_case("true")),
            mission_name: text("missionName"),
            mission_guid: text("missionGuid"),
            timestamp: text("timestamp"),
            creator_uid: text("creatorUid"),
            content_uid: text("contentUid"),
            other: element
                .elements()
                .filter(|child| !Self::KNOWN.contains(&child.name.as_str()))
                .cloned()
                .collect(),
        }
    }

    /// Renders a `<MissionChange>` element in TAK Server's field order.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut element = Element::new("MissionChange");
        let mut push = |name: &str, value: Option<String>| {
            if let Some(value) = value {
                element.push(Element::new(name).with(Node::Text(value)));
            }
        };
        push("type", self.r#type.clone());
        push(
            "isFederatedChange",
            self.is_federated_change.map(|v| v.to_string()),
        );
        push("missionName", self.mission_name.clone());
        push("missionGuid", self.mission_guid.clone());
        push("timestamp", self.timestamp.clone());
        push("creatorUid", self.creator_uid.clone());
        push("contentUid", self.content_uid.clone());
        for child in &self.other {
            element.push(child.clone());
        }
        element
    }
}

/// `<mission>` — the notification body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MissionDetail {
    /// What happened.
    pub r#type: MissionNotice,
    /// The mission's tool, e.g. `public`. TAK emits this even when empty.
    pub tool: Option<String>,
    /// Mission name.
    pub name: Option<String>,
    /// Mission GUID.
    pub guid: Option<String>,
    /// Who triggered the notification.
    pub author_uid: Option<String>,
    /// Content uid, when the notice is about one item.
    pub uid: Option<String>,
    /// Subscription or invitation token.
    pub token: Option<String>,
    /// The `<MissionChanges>` payload, in document order.
    pub changes: Vec<MissionChange>,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl MissionDetail {
    const KNOWN: &'static [&'static str] =
        &["type", "tool", "name", "guid", "authorUid", "uid", "token"];

    /// A change notice for a named mission.
    #[must_use]
    pub fn change(name: impl Into<String>, guid: impl Into<String>) -> Self {
        Self {
            r#type: MissionNotice::Change,
            name: Some(name.into()),
            guid: Some(guid.into()),
            tool: Some(String::new()),
            ..Self::default()
        }
    }
}

impl TypedDetail for MissionDetail {
    const NAME: &'static str = "mission";

    fn from_element(element: &Element) -> Option<Self> {
        let attr = |name: &str| element.get(name).map(ToOwned::to_owned);
        Some(Self {
            r#type: MissionNotice::from(element.get("type").unwrap_or_default()),
            tool: attr("tool"),
            name: attr("name"),
            guid: attr("guid"),
            author_uid: attr("authorUid"),
            uid: attr("uid"),
            token: attr("token"),
            changes: element
                .child("MissionChanges")
                .into_iter()
                .flat_map(Element::elements)
                .filter(|child| child.name == "MissionChange")
                .map(MissionChange::from_element)
                .collect(),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr_opt("name", self.name.clone())
            .attr_opt("guid", self.guid.clone())
            .attr("type", self.r#type.as_str())
            .attr_opt("authorUid", self.author_uid.clone())
            .attr_opt("tool", self.tool.clone())
            .attr_opt("uid", self.uid.clone())
            .attr_opt("token", self.token.clone());
        apply_extras(&mut element, &self.extra);
        if !self.changes.is_empty() {
            let mut changes = Element::new("MissionChanges");
            for change in &self.changes {
                changes.push(change.to_element());
            }
            element.push(changes);
        }
        element
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_types_round_trip_including_unknown_ones() {
        for (text, notice) in [
            ("CHANGE", MissionNotice::Change),
            ("INVITE", MissionNotice::Invite),
            ("CREATE", MissionNotice::Create),
            ("DELETE", MissionNotice::Delete),
        ] {
            assert_eq!(MissionNotice::from(text), notice);
            assert_eq!(notice.as_str(), text);
        }
        assert_eq!(
            MissionNotice::from("ROLE"),
            MissionNotice::Other("ROLE".into())
        );
        assert_eq!(MissionNotice::from("ROLE").as_str(), "ROLE");
    }

    #[test]
    fn a_change_notice_renders_the_documented_attribute_set() {
        let detail = MissionDetail::change("Recon", "GUID-1");
        let element = detail.to_element();
        assert_eq!(
            element.attrs,
            vec![
                ("name".into(), "Recon".into()),
                ("guid".into(), "GUID-1".into()),
                ("type".into(), "CHANGE".into()),
                ("tool".into(), String::new()),
            ]
        );
        assert!(element.children.is_empty());
        assert_eq!(MissionDetail::from_element(&element), Some(detail));
    }

    #[test]
    fn mission_changes_are_child_elements_not_attributes() {
        let mut detail = MissionDetail::change("Recon", "GUID-1");
        detail.changes.push(MissionChange {
            r#type: Some("ADD_CONTENT".into()),
            mission_name: Some("Recon".into()),
            content_uid: Some("UID-A".into()),
            is_federated_change: Some(false),
            ..MissionChange::default()
        });

        let element = detail.to_element();
        let change = element
            .child("MissionChanges")
            .unwrap()
            .child("MissionChange")
            .unwrap();
        assert_eq!(change.attrs, vec![]);
        assert_eq!(change.child("type").unwrap().text(), "ADD_CONTENT");
        assert_eq!(change.child("contentUid").unwrap().text(), "UID-A");
        assert_eq!(MissionDetail::from_element(&element), Some(detail));
    }

    #[test]
    fn unmodelled_change_children_are_kept_verbatim() {
        let element = Element::new("mission").attr("type", "CHANGE").with(
            Element::new("MissionChanges").with(
                Element::new("MissionChange")
                    .with(Element::new("type").with(Node::Text("ADD_CONTENT".into())))
                    .with(Element::new("contentResource").attr("hash", "abc")),
            ),
        );
        let detail = MissionDetail::from_element(&element).unwrap();
        assert_eq!(detail.changes[0].other.len(), 1);
        assert_eq!(detail.changes[0].other[0].name, "contentResource");
        assert_eq!(
            detail.to_element().child("MissionChanges").unwrap(),
            element.child("MissionChanges").unwrap()
        );
    }

    #[test]
    fn an_invite_keeps_its_token_and_unmodelled_attributes() {
        let element = Element::new("mission")
            .attr("type", "INVITE")
            .attr("name", "Recon")
            .attr("token", "eyJ0")
            .attr("role", "MISSION_SUBSCRIBER");
        let detail = MissionDetail::from_element(&element).unwrap();
        assert_eq!(detail.r#type, MissionNotice::Invite);
        assert_eq!(detail.token.as_deref(), Some("eyJ0"));
        assert_eq!(
            detail.extra,
            vec![("role".into(), "MISSION_SUBSCRIBER".into())]
        );
        assert_eq!(detail.to_element().get("role"), Some("MISSION_SUBSCRIBER"));
    }
}
