//! `<marti>` — explicit addressing, and the rules for reading it.
//!
//! A sender addresses specific recipients by hanging `<dest>` children off a
//! `<marti>` element. Two behaviours are load-bearing for interoperability:
//!
//! * **First attribute wins.** A `<dest>` is matched against its attributes in
//!   the fixed order callsign → publish → uid → mission → mission-guid →
//!   group, and only the first match is honoured. See [`Dest::kind`].
//! * **`<marti>` is always stripped before relay**, so a recipient never sees
//!   who else a message was addressed to. See [`take_marti`].

use super::{Detail, Element};

/// The pseudo-callsign that degrades an explicit list to a broadcast.
///
/// When any `<dest callsign>` in a message carries this value the whole
/// callsign list is discarded and the message is broadcast to everyone the
/// sender can reach.
pub const ALL_STREAMING: &str = "All Streaming";

/// The attributes that make a `<dest>` worth looking at.
const ROUTING_ATTRS: &[&str] = &[
    "callsign",
    "publish",
    "uid",
    "mission",
    "mission-guid",
    "path",
    "after",
    "group",
];

/// One `<dest>` child of `<marti>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dest {
    /// Deliver to the connection with this callsign.
    pub callsign: Option<String>,
    /// Publish to a broker topic. Never implemented by TAK Server.
    pub publish: Option<String>,
    /// Deliver to the connection with this client uid.
    pub uid: Option<String>,
    /// Publish into the mission with this name.
    pub mission: Option<String>,
    /// Publish into the mission with this GUID.
    pub mission_guid: Option<String>,
    /// Layer path within the mission. Only meaningful with a mission.
    pub path: Option<String>,
    /// Place after this content uid. Only honoured when `path` is set too.
    pub after: Option<String>,
    /// Re-address the message to this channel; the sender must hold it IN.
    pub group: Option<String>,
}

/// The single routing decision a [`Dest`] expresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestKind<'a> {
    /// Deliver to a callsign.
    Callsign(&'a str),
    /// Publish to a topic (accepted, never routed).
    Publish(&'a str),
    /// Deliver to a client uid.
    Uid(&'a str),
    /// Publish into a mission by name.
    Mission {
        /// Mission name.
        name: &'a str,
        /// Layer path.
        path: Option<&'a str>,
        /// Sibling to insert after.
        after: Option<&'a str>,
    },
    /// Publish into a mission by GUID.
    MissionGuid {
        /// Mission GUID.
        guid: &'a str,
        /// Layer path.
        path: Option<&'a str>,
        /// Sibling to insert after.
        after: Option<&'a str>,
    },
    /// Re-address to a channel.
    Group(&'a str),
}

impl Dest {
    /// Addresses a callsign.
    #[must_use]
    pub fn callsign(callsign: impl Into<String>) -> Self {
        Self {
            callsign: Some(callsign.into()),
            ..Self::default()
        }
    }

    /// Addresses a client uid.
    #[must_use]
    pub fn uid(uid: impl Into<String>) -> Self {
        Self {
            uid: Some(uid.into()),
            ..Self::default()
        }
    }

    /// Addresses a mission by name.
    #[must_use]
    pub fn mission(name: impl Into<String>) -> Self {
        Self {
            mission: Some(name.into()),
            ..Self::default()
        }
    }

    /// Addresses a mission by GUID.
    #[must_use]
    pub fn mission_guid(guid: impl Into<String>) -> Self {
        Self {
            mission_guid: Some(guid.into()),
            ..Self::default()
        }
    }

    /// Addresses a channel.
    #[must_use]
    pub fn group(name: impl Into<String>) -> Self {
        Self {
            group: Some(name.into()),
            ..Self::default()
        }
    }

    /// The routing decision, applying first-attribute-wins order.
    ///
    /// Returns [`None`] for a `<dest>` that carries only `path`/`after`, which
    /// selects the element but names no destination.
    #[must_use]
    pub fn kind(&self) -> Option<DestKind<'_>> {
        let path = self.path.as_deref();
        // TAK Server reads `after` only when `path` is present.
        let after = path.and(self.after.as_deref());

        if let Some(callsign) = self.callsign.as_deref() {
            Some(DestKind::Callsign(callsign))
        } else if let Some(publish) = self.publish.as_deref() {
            Some(DestKind::Publish(publish))
        } else if let Some(uid) = self.uid.as_deref() {
            Some(DestKind::Uid(uid))
        } else if let Some(name) = self.mission.as_deref() {
            Some(DestKind::Mission { name, path, after })
        } else if let Some(guid) = self.mission_guid.as_deref() {
            Some(DestKind::MissionGuid { guid, path, after })
        } else {
            self.group.as_deref().map(DestKind::Group)
        }
    }

    /// Whether this dest addresses everyone rather than one callsign.
    #[must_use]
    pub fn is_all_streaming(&self) -> bool {
        self.callsign.as_deref() == Some(ALL_STREAMING)
    }

    /// Reads a `<dest>` element.
    #[must_use]
    pub fn from_element(element: &Element) -> Self {
        let attr = |name: &str| element.get(name).map(ToOwned::to_owned);
        Self {
            callsign: attr("callsign"),
            publish: attr("publish"),
            uid: attr("uid"),
            mission: attr("mission"),
            mission_guid: attr("mission-guid"),
            path: attr("path"),
            after: attr("after"),
            group: attr("group"),
        }
    }

    /// Renders a `<dest>` element in canonical attribute order.
    #[must_use]
    pub fn to_element(&self) -> Element {
        Element::new("dest")
            .attr_opt("callsign", self.callsign.clone())
            .attr_opt("publish", self.publish.clone())
            .attr_opt("uid", self.uid.clone())
            .attr_opt("mission", self.mission.clone())
            .attr_opt("mission-guid", self.mission_guid.clone())
            .attr_opt("path", self.path.clone())
            .attr_opt("after", self.after.clone())
            .attr_opt("group", self.group.clone())
    }

    /// Whether the element carries at least one attribute TAK Server selects on.
    fn selects(element: &Element) -> bool {
        element
            .attrs
            .iter()
            .any(|(name, _)| ROUTING_ATTRS.contains(&name.as_str()))
    }
}

/// Removes **every** `<marti>` element and returns the destinations it held.
///
/// Stripping is unconditional, matching TAK Server: even a `<marti>` with no
/// usable `<dest>` is removed, so recipients never learn the address list.
/// Only `<dest>` children carrying a routing attribute are returned.
pub fn take_marti(detail: &mut Detail) -> Vec<Dest> {
    detail
        .remove_all("marti")
        .iter()
        .flat_map(Element::elements)
        .filter(|element| element.name == "dest" && Dest::selects(element))
        .map(Dest::from_element)
        .collect()
}

/// Reads the destinations without removing anything.
#[must_use]
pub fn read_marti(detail: &Detail) -> Vec<Dest> {
    detail
        .find_all("marti")
        .into_iter()
        .flat_map(Element::elements)
        .filter(|element| element.name == "dest" && Dest::selects(element))
        .map(Dest::from_element)
        .collect()
}

/// Builds `<marti><dest …/>…</marti>` for the client side.
#[must_use]
pub fn marti_element(dests: &[Dest]) -> Element {
    let mut marti = Element::new("marti");
    for dest in dests {
        marti.push(dest.to_element());
    }
    marti
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attribute_wins_in_the_documented_order() {
        let every = Dest {
            callsign: Some("ALPHA".into()),
            publish: Some("topic".into()),
            uid: Some("UID".into()),
            mission: Some("Mission".into()),
            mission_guid: Some("GUID".into()),
            group: Some("Blue".into()),
            ..Dest::default()
        };
        assert_eq!(every.kind(), Some(DestKind::Callsign("ALPHA")));

        let mut without = every.clone();
        without.callsign = None;
        assert_eq!(without.kind(), Some(DestKind::Publish("topic")));
        without.publish = None;
        assert_eq!(without.kind(), Some(DestKind::Uid("UID")));
        without.uid = None;
        assert_eq!(
            without.kind(),
            Some(DestKind::Mission {
                name: "Mission",
                path: None,
                after: None
            })
        );
        without.mission = None;
        assert_eq!(
            without.kind(),
            Some(DestKind::MissionGuid {
                guid: "GUID",
                path: None,
                after: None
            })
        );
        without.mission_guid = None;
        assert_eq!(without.kind(), Some(DestKind::Group("Blue")));
        without.group = None;
        assert_eq!(without.kind(), None);
    }

    #[test]
    fn after_is_ignored_unless_path_is_present() {
        let mut dest = Dest::mission("Recon");
        dest.after = Some("uid-1".into());
        assert_eq!(
            dest.kind(),
            Some(DestKind::Mission {
                name: "Recon",
                path: None,
                after: None
            })
        );

        dest.path = Some("layer".into());
        assert_eq!(
            dest.kind(),
            Some(DestKind::Mission {
                name: "Recon",
                path: Some("layer"),
                after: Some("uid-1")
            })
        );
    }

    #[test]
    fn take_marti_strips_every_element_and_keeps_routable_dests() {
        let mut detail = Detail::new();
        detail.push(
            Element::new("marti")
                .with(Element::new("dest").attr("callsign", "ALPHA"))
                .with(Element::new("dest").attr("phone", "555"))
                .with(Element::new("notdest").attr("uid", "X")),
        );
        detail.push(Element::new("contact").attr("callsign", "BRAVO"));
        detail.push(Element::new("marti").with(Element::new("dest").attr("uid", "UID-2")));

        let dests = take_marti(&mut detail);
        assert_eq!(dests.len(), 2);
        assert_eq!(dests[0].kind(), Some(DestKind::Callsign("ALPHA")));
        assert_eq!(dests[1].kind(), Some(DestKind::Uid("UID-2")));
        assert_eq!(detail.count("marti"), 0);
        assert_eq!(detail.count("contact"), 1);
    }

    #[test]
    fn an_empty_marti_is_still_stripped() {
        let mut detail = Detail::new();
        detail.push(Element::new("marti"));
        assert!(take_marti(&mut detail).is_empty());
        assert!(detail.is_empty());
    }

    #[test]
    fn a_path_only_dest_is_selected_but_names_nothing() {
        let mut detail = Detail::new();
        detail.push(Element::new("marti").with(Element::new("dest").attr("path", "layer")));
        let dests = take_marti(&mut detail);
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].kind(), None);
    }

    #[test]
    fn all_streaming_is_recognised_verbatim() {
        assert!(Dest::callsign(ALL_STREAMING).is_all_streaming());
        assert!(!Dest::callsign("all streaming").is_all_streaming());
    }

    #[test]
    fn elements_round_trip_in_canonical_order() {
        let dest = Dest {
            mission: Some("Recon".into()),
            path: Some("layer".into()),
            after: Some("uid-1".into()),
            ..Dest::default()
        };
        let element = dest.to_element();
        assert_eq!(
            element.attrs,
            vec![
                ("mission".into(), "Recon".into()),
                ("path".into(), "layer".into()),
                ("after".into(), "uid-1".into()),
            ]
        );
        assert_eq!(Dest::from_element(&element), dest);

        let marti = marti_element(&[Dest::callsign("A"), Dest::uid("B")]);
        assert_eq!(marti.name, "marti");
        assert_eq!(marti.elements().count(), 2);
    }

    #[test]
    fn read_marti_leaves_the_tree_alone() {
        let mut detail = Detail::new();
        detail.push(marti_element(&[Dest::callsign("ALPHA")]));
        assert_eq!(read_marti(&detail).len(), 1);
        assert_eq!(detail.count("marti"), 1);
    }
}
