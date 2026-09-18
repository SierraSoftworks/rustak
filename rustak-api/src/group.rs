//! Channels, which TAK calls groups, and who may use them.

use serde::{Deserialize, Serialize};

use crate::identity::{Direction, GroupId, GroupName, Username};

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

/// One member of one channel, as the channel's own listing describes them.
///
/// The mirror image of [`GroupMembership`]: that answers "which channels does
/// this person hold", this answers "who holds this channel". Both are one row
/// per single direction, because that is what storage holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMember {
    pub username: Username,

    /// `In` or `Out`; never [`Direction::Both`].
    pub direction: Direction,

    /// Whether the member currently has this channel switched **on**.
    ///
    /// A membership is a right and this is a preference — the one a client
    /// sets through `PUT /Marti/api/groups/active`. A member who has never
    /// said anything counts as on, so this is `true` far more often than it is
    /// stored.
    #[serde(default = "crate::group::on")]
    pub active: bool,

    /// Why the membership exists, so the UI can grey out the ones the identity
    /// provider will overwrite at the member's next sign-in.
    #[serde(default)]
    pub source: MembershipSource,
}

/// The default for [`GroupMember::active`]: a channel nobody has switched off.
fn on() -> bool {
    true
}

/// A request to create a channel.
///
/// The bit position is not here: it is the server's to allocate, and a client
/// that could choose one could hand an existing channel's traffic to a new set
/// of members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateGroupRequest {
    pub name: GroupName,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A change to a channel.
///
/// Only the description: a channel's name is what every membership, every
/// `groups` claim and every client's cached selection refers to, so renaming
/// one is deleting it and making another.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GroupPatch {
    /// The new description. An empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl GroupPatch {
    /// Whether this patch would change anything.
    pub fn is_empty(&self) -> bool {
        self.description.is_none()
    }
}

/// Whether one device currently has a channel switched on.
///
/// A membership is a right and this is a preference: switching a channel off on
/// a phone must not switch it off on a laptop, so the state is scoped to the
/// device that asked. A channel a device has said nothing about counts as on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGroup {
    pub group: GroupName,

    /// The direction being switched. [`Direction::Both`] stands for the two
    /// rows storage holds, exactly as it does for a membership.
    pub direction: Direction,

    pub active: bool,
}

impl ActiveGroup {
    /// The single-direction states this stands for, which is what storage
    /// holds.
    pub fn expand(&self) -> impl Iterator<Item = Self> + '_ {
        self.direction.expand().iter().map(|direction| Self {
            group: self.group.clone(),
            direction: *direction,
            active: self.active,
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
    #[test]
    fn a_request_to_create_a_channel_cannot_choose_its_bit_position() {
        // Choosing one would let a new channel be handed an existing channel's
        // traffic, so the field is deliberately absent rather than ignored.
        let request: CreateGroupRequest = serde_json::from_value(serde_json::json!({
            "name": "Blue",
        }))
        .unwrap();

        assert_eq!(request.description, None);
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"name":"Blue"}"#
        );

        assert!(
            serde_json::from_value::<CreateGroupRequest>(serde_json::json!({
                "name": "Blue",
                "bitpos": 7,
            }))
            .is_ok(),
            "an unknown field is ignored rather than refused, but must not be read",
        );
    }

    #[test]
    fn an_empty_patch_would_change_nothing() {
        let patch: GroupPatch = serde_json::from_value(serde_json::json!({})).unwrap();

        assert!(patch.is_empty());
        assert!(
            !GroupPatch {
                description: Some(String::new()),
            }
            .is_empty(),
            "clearing the description is a change",
        );
    }

    #[test]
    fn an_active_state_of_both_directions_expands_to_the_rows_storage_holds() {
        let both = ActiveGroup {
            group: GroupName::parse("Blue").unwrap(),
            direction: Direction::Both,
            active: false,
        };

        let expanded: Vec<Direction> = both.expand().map(|state| state.direction).collect();
        assert_eq!(expanded, vec![Direction::In, Direction::Out]);
        assert!(both.expand().all(|state| !state.active));

        let json = serde_json::to_string(&both).unwrap();
        assert_eq!(
            json,
            r#"{"group":"Blue","direction":"BOTH","active":false}"#
        );
        assert_eq!(serde_json::from_str::<ActiveGroup>(&json).unwrap(), both);
    }

    #[test]
    fn a_member_of_a_channel_is_on_unless_they_have_said_otherwise() {
        let member: GroupMember = serde_json::from_value(serde_json::json!({
            "username": "grace",
            "direction": "OUT",
        }))
        .unwrap();

        assert!(member.active);
        assert_eq!(member.source, MembershipSource::Manual);

        let switched_off = GroupMember {
            active: false,
            source: MembershipSource::Oidc,
            ..member
        };
        let json = serde_json::to_string(&switched_off).unwrap();

        assert_eq!(
            serde_json::from_str::<GroupMember>(&json).unwrap(),
            switched_off
        );
    }
}
