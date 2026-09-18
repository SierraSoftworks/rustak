//! Channels, which TAK calls groups, and who may use them.

use serde::{Deserialize, Serialize};

use crate::identity::{Direction, GroupId, GroupName};

/// Where a channel came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupSource {
    /// Created by the server itself, and not removable.
    /// [`GroupName::ANON`] is the one of these every installation has.
    System,

    /// Created by an administrator.
    #[default]
    Manual,

    /// Created because the identity provider named it in a `groups` claim.
    Oidc,
}

impl GroupSource {
    /// Every source, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::System, Self::Manual, Self::Oidc];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Manual => "manual",
            Self::Oidc => "oidc",
        }
    }

    /// A short phrase naming the source for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::System => "Built in",
            Self::Manual => "Created here",
            Self::Oidc => "From single sign-on",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.as_str() == value)
    }

    /// Whether an administrator may rename or remove this channel.
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Manual)
    }
}

/// Why somebody is a member of a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipSource {
    /// An administrator granted it.
    #[default]
    Manual,

    /// It was mapped from a `groups` claim at sign-in, and is replaced wholesale
    /// the next time that person signs in — so editing it here would not last.
    Oidc,
}

impl MembershipSource {
    /// Every source, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Manual, Self::Oidc];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Oidc => "oidc",
        }
    }

    /// A short phrase naming the source for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Manual => "Granted here",
            Self::Oidc => "From single sign-on",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.as_str() == value)
    }

    /// Whether an administrator may change this membership and have the change
    /// survive the member's next sign-in.
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Manual)
    }
}

/// A channel as described to the admin UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: GroupName,

    /// This channel's index in the bit vector the router uses to decide who
    /// receives a message.
    ///
    /// Allocated by the server and never reused while anything is still
    /// subscribed, because a reused position would silently hand one channel's
    /// traffic to another channel's members. TAK drops any channel whose bit
    /// position is negative, so this is unsigned here.
    pub bitpos: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default)]
    pub source: GroupSource,
}

/// One person's access to one channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMembership {
    pub group: GroupName,

    /// Storage holds one row per single direction, so a membership read back
    /// from the database is never [`Direction::Both`] — but a grant sent by the
    /// UI may be, and the server expands it.
    pub direction: Direction,

    /// Why the membership exists. Absent when the caller did not ask for it,
    /// present in an administrator's listing so the UI can grey out the
    /// memberships that the identity provider will overwrite anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<MembershipSource>,
}

impl GroupMembership {
    /// A grant an administrator is making by hand.
    pub fn new(group: GroupName, direction: Direction) -> Self {
        Self {
            group,
            direction,
            source: None,
        }
    }

    /// The single-direction memberships this grant stands for, which is what
    /// storage holds.
    pub fn expand(&self) -> impl Iterator<Item = Self> + '_ {
        self.direction.expand().iter().map(|direction| Self {
            group: self.group.clone(),
            direction: *direction,
            source: self.source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_round_trips_through_serde() {
        let group = Group {
            id: GroupId::new(1),
            name: GroupName::anon(),
            bitpos: 1,
            description: Some("Everybody".into()),
            source: GroupSource::System,
        };

        let json = serde_json::to_string(&group).unwrap();
        assert_eq!(serde_json::from_str::<Group>(&json).unwrap(), group);
    }

    #[test]
    fn a_channel_with_no_description_omits_it_and_defaults_to_manual() {
        let group: Group = serde_json::from_value(serde_json::json!({
            "id": 2,
            "name": "Blue",
            "bitpos": 2,
        }))
        .unwrap();

        assert_eq!(group.source, GroupSource::Manual);
        assert_eq!(group.description, None);
        assert_eq!(
            serde_json::to_string(&group).unwrap(),
            r#"{"id":2,"name":"Blue","bitpos":2,"source":"manual"}"#
        );
    }

    #[test]
    fn a_membership_round_trips_through_serde() {
        let membership = GroupMembership {
            group: GroupName::parse("Blue").unwrap(),
            direction: Direction::Out,
            source: Some(MembershipSource::Oidc),
        };

        let json = serde_json::to_string(&membership).unwrap();
        assert_eq!(
            json,
            r#"{"group":"Blue","direction":"OUT","source":"oidc"}"#
        );
        assert_eq!(
            serde_json::from_str::<GroupMembership>(&json).unwrap(),
            membership
        );
    }

    #[test]
    fn a_grant_of_both_directions_expands_to_the_rows_storage_holds() {
        let grant = GroupMembership::new(GroupName::parse("Blue").unwrap(), Direction::Both);
        let expanded: Vec<Direction> = grant.expand().map(|m| m.direction).collect();

        assert_eq!(expanded, vec![Direction::In, Direction::Out]);

        let single = GroupMembership::new(GroupName::parse("Blue").unwrap(), Direction::In);
        assert_eq!(single.expand().count(), 1);
    }

    #[test]
    fn sources_round_trip_through_their_wire_form() {
        for source in GroupSource::ALL.iter().copied() {
            let json = serde_json::to_string(&source).unwrap();

            assert_eq!(json, format!("\"{}\"", source.as_str()));
            assert_eq!(serde_json::from_str::<GroupSource>(&json).unwrap(), source);
            assert_eq!(GroupSource::parse(source.as_str()), Some(source));
        }

        for source in MembershipSource::ALL.iter().copied() {
            let json = serde_json::to_string(&source).unwrap();

            assert_eq!(json, format!("\"{}\"", source.as_str()));
            assert_eq!(
                serde_json::from_str::<MembershipSource>(&json).unwrap(),
                source
            );
            assert_eq!(MembershipSource::parse(source.as_str()), Some(source));
        }

        // The built-in channel cannot be edited away, and a membership the
        // identity provider owns cannot be edited to stick.
        assert!(!GroupSource::System.is_editable());
        assert!(GroupSource::Manual.is_editable());
        assert!(!MembershipSource::Oidc.is_editable());
    }
}
