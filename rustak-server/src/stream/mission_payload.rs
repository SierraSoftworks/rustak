//! The payloads a `t-x-m-*` notice carries inside `<mission>`.
//!
//! Split from [`mission_notify`](super::mission_notify) because it is a
//! different kind of thing: that module decides which notice is being sent and
//! to whom, and this one is the vocabulary those notices are written in — the
//! change list, the file description, the layer and the role.
//!
//! # Two conventions in one document
//!
//! Everything under `<MissionChanges>` is a **child element**; `<details>`
//! inside one of those changes is **attributes** with a `<location>` child;
//! `<mission>` itself is attributes. That is what TAK Server's JAXB bindings
//! produce and what CloudTAK's parser expects, so it is reproduced rather than
//! made consistent.

use chrono::{DateTime, Utc};
use rustak_api::MissionRoleKind;
use rustak_cot::detail::{Element, Node};

use crate::marti::time::cot_date;

use super::mission_notify::NoticeMission;

/// The cached description of a map item, rendered as `<details …/>`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UidDetailsXml {
    /// The CoT type of the item.
    pub kind: Option<String>,
    /// Its callsign, from `<contact>`.
    pub callsign: Option<String>,
    /// Its title, when it has one.
    pub title: Option<String>,
    /// `<usericon iconsetpath>`.
    pub iconset_path: Option<String>,
    /// `<color argb>`.
    pub color: Option<String>,
    /// Where it was when it was filed.
    pub location: Option<(f64, f64)>,
}

impl UidDetailsXml {
    /// `<details type callsign color iconsetPath title><location lat lon/></details>`.
    pub(super) fn to_element(&self) -> Element {
        let mut element = Element::new("details")
            .attr_opt("type", self.kind.clone())
            .attr_opt("callsign", self.callsign.clone())
            .attr_opt("color", self.color.clone())
            .attr_opt("iconsetPath", self.iconset_path.clone())
            .attr_opt("title", self.title.clone());

        if let Some((lat, lon)) = self.location {
            element.push(
                Element::new("location")
                    .attr("lat", lat.to_string())
                    .attr("lon", lon.to_string()),
            );
        }

        element
    }
}

/// `<contentResource>` — a stored file, with every field as a child element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceXml {
    /// Who uploaded it.
    pub creator_uid: String,
    /// Epoch millis, or `-1` for never.
    pub expiration: i64,
    /// The name it was uploaded under.
    pub filename: String,
    /// Its content hash, which is also its identifier.
    pub hash: String,
    /// Search keywords.
    pub keywords: Vec<String>,
    /// Its media type.
    pub mime_type: String,
    /// The display name, which need not be the filename.
    pub name: String,
    /// How large it is.
    pub size: u64,
    /// When it was stored.
    pub submission_time: DateTime<Utc>,
    /// The account that stored it.
    pub submitter: String,
    /// The tool it belongs to.
    pub tool: String,
    /// The item it is an attachment of, when it is one.
    pub uid: String,
}

impl ResourceXml {
    /// The element, with its children in TAK's alphabetical order.
    pub(super) fn to_element(&self) -> Element {
        let mut element = Element::new("contentResource");
        let text = |name: &str, value: &str| Element::new(name).with(Node::Text(value.to_owned()));

        element.push(text("creatorUid", &self.creator_uid));
        element.push(text("expiration", &self.expiration.to_string()));
        element.push(text("filename", &self.filename));
        element.push(text("hash", &self.hash));
        for keyword in &self.keywords {
            element.push(text("keywords", keyword));
        }
        element.push(text("mimeType", &self.mime_type));
        element.push(text("name", &self.name));
        element.push(text("size", &self.size.to_string()));
        element.push(text("submissionTime", &cot_date(self.submission_time)));
        element.push(text("submitter", &self.submitter));
        element.push(text("tool", &self.tool));
        element.push(text("uid", &self.uid));

        element
    }
}

/// One `<MissionChange>`, as a notice carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct MissionChangeXml {
    /// `ADD_CONTENT`, `REMOVE_CONTENT`, `CREATE_MISSION`, …
    pub kind: String,
    /// The uid of the map item that changed.
    pub content_uid: Option<String>,
    /// Who changed it.
    pub creator_uid: Option<String>,
    /// When.
    pub timestamp: DateTime<Utc>,
    /// The cached description of the item, when the change is about a uid.
    pub details: Option<UidDetailsXml>,
    /// The stored file, when the change is about one.
    pub resource: Option<ResourceXml>,
}

impl MissionChangeXml {
    /// A change of a given kind at a given moment.
    pub fn new(kind: impl Into<String>, timestamp: DateTime<Utc>) -> Self {
        Self {
            kind: kind.into(),
            content_uid: None,
            creator_uid: None,
            timestamp,
            details: None,
            resource: None,
        }
    }

    /// The `rustak-cot` model this renders through.
    pub(super) fn to_change(&self, mission: &NoticeMission) -> rustak_cot::detail::MissionChange {
        let mut other = Vec::new();
        if let Some(details) = &self.details {
            other.push(details.to_element());
        }
        if let Some(resource) = &self.resource {
            other.push(resource.to_element());
        }

        rustak_cot::detail::MissionChange {
            r#type: Some(self.kind.clone()),
            is_federated_change: Some(false),
            mission_name: Some(mission.name.clone()),
            mission_guid: Some(mission.guid.clone()),
            timestamp: Some(cot_date(self.timestamp)),
            creator_uid: self.creator_uid.clone(),
            content_uid: self.content_uid.clone(),
            other,
        }
    }
}

/// `<missionLayer name parentUid type uid/>`, the child a `t-x-m-c-h` carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MissionLayerXml {
    /// The layer's identifier.
    pub uid: String,
    /// Its label.
    pub name: Option<String>,
    /// `GROUP`, `UID`, `CONTENTS`, `MAPLAYER` or `ITEM`.
    pub kind: String,
    /// The layer it hangs under, when it is not a root.
    pub parent_uid: Option<String>,
}

impl MissionLayerXml {
    pub(super) fn to_element(&self) -> Element {
        Element::new("missionLayer")
            .attr_opt("name", self.name.clone())
            .attr_opt("parentUid", self.parent_uid.clone())
            .attr("type", self.kind.clone())
            .attr("uid", self.uid.clone())
    }
}

/// `<role type=…><permissions>MISSION_READ</permissions>…</role>`.
///
/// **Repeated text elements**, not a wrapper with typed children. TAK Server's
/// `MissionRole` is `@XmlElement(name="permissions")` on a `Set<String>`
/// (research `05` §7.5), which JAXB renders as one `<permissions>` per
/// permission with the name as its character data. M4-02 deviation 5 chose
/// design 04 §4.8's nested `<permission type=…/>` shape instead; per
/// `compat/README.md` the research is authoritative over the design, and a
/// client reading `role/permissions` text got nothing at all from the nested
/// form (R-02 M11). The deviation is withdrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissionRoleXml(pub MissionRoleKind);

impl MissionRoleXml {
    /// What each role is allowed to do, most privileged first.
    ///
    /// Duplicated from `missions/roles.rs` on purpose: a notice is rendered on
    /// the stream side, which has no business reaching into the mission
    /// service, and the eight names are a wire vocabulary rather than a policy.
    pub fn permissions(&self) -> &'static [&'static str] {
        match self.0 {
            MissionRoleKind::Owner => &[
                "MISSION_READ",
                "MISSION_WRITE",
                "MISSION_DELETE",
                "MISSION_SET_ROLE",
                "MISSION_SET_PASSWORD",
                "MISSION_UPDATE_GROUPS",
                "MISSION_MANAGE_FEEDS",
                "MISSION_MANAGE_LAYERS",
            ],
            MissionRoleKind::Subscriber => &["MISSION_READ", "MISSION_WRITE"],
            MissionRoleKind::ReadonlySubscriber => &["MISSION_READ"],
        }
    }

    pub(super) fn to_element(self) -> Element {
        let mut role = Element::new("role").attr("type", self.0.as_str());

        for permission in self.permissions() {
            role.push(
                Element::new("permissions").with(rustak_cot::Node::Text((*permission).to_string())),
            );
        }

        role
    }
}
