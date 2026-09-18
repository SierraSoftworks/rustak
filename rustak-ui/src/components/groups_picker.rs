//! Which channels an account may write into and read out of.
//!
//! Every channel the installation has is listed, held or not, because the
//! question an operator is answering is "who should be in this" rather than
//! "what is this person already in" — and a list that showed only the
//! memberships already granted would have no row to tick.
//!
//! # `IN` and `OUT` are not what most people guess
//!
//! TAK names the directions from the server's point of view: `IN` is traffic
//! arriving, so it is permission to *write*, and `OUT` is traffic leaving, so it
//! is permission to *read*. The columns are labelled the way the person reading
//! them thinks — Write and Read — with the wire name in the header's tooltip, so
//! nothing has to be learned to use the page and nothing is hidden from somebody
//! comparing it against TAK's own API.

use std::collections::BTreeMap;

use rustak_api::{Direction, Group, GroupMembership, GroupName, MembershipSource};
use yew::prelude::*;

use crate::components::Switch;

#[derive(Properties, PartialEq)]
pub struct GroupsPickerProps {
    /// Every channel this installation has.
    pub groups: Vec<Group>,

    /// What is currently held, in whatever form the server sent it — one row
    /// per single direction, or a `BOTH` the UI itself made.
    pub value: Vec<GroupMembership>,

    /// The whole set after a toggle, ready to be `PUT` back.
    pub onchange: Callback<Vec<GroupMembership>>,

    #[prop_or_default]
    pub disabled: bool,

    /// Distinguishes this picker's control ids from any other on the page.
    #[prop_or(AttrValue::from("channels"))]
    pub id_prefix: AttrValue,
}

/// One channel's row in the picker.
#[derive(Clone, Copy, Default, PartialEq)]
struct Held {
    write: bool,
    read: bool,
    /// Whether the identity provider owns this membership, in which case
    /// changing it here would last until the member next signed in and no
    /// longer — so it is shown and not offered.
    claimed: bool,
}

impl Held {
    /// The grant this stands for, or nothing when the channel is not held.
    fn direction(self) -> Option<Direction> {
        match (self.write, self.read) {
            (true, true) => Some(Direction::Both),
            (true, false) => Some(Direction::In),
            (false, true) => Some(Direction::Out),
            (false, false) => None,
        }
    }
}

/// Folds the server's per-direction rows into one entry per channel.
fn fold(value: &[GroupMembership]) -> BTreeMap<String, Held> {
    let mut held: BTreeMap<String, Held> = BTreeMap::new();

    for grant in value {
        let entry = held.entry(grant.group.to_string()).or_default();
        entry.write |= grant.direction.includes(Direction::In);
        entry.read |= grant.direction.includes(Direction::Out);
        entry.claimed |= grant.source == Some(MembershipSource::Oidc);
    }

    held
}

/// Turns the folded rows back into the set the server is sent.
fn unfold(held: &BTreeMap<String, Held>) -> Vec<GroupMembership> {
    held.iter()
        .filter(|(_, entry)| !entry.claimed)
        .filter_map(|(name, entry)| {
            entry.direction().map(|direction| GroupMembership {
                group: GroupName::from_storage(name.clone()),
                direction,
                source: None,
            })
        })
        .collect()
}

#[function_component(GroupsPicker)]
pub fn groups_picker(props: &GroupsPickerProps) -> Html {
    let held = fold(&props.value);

    let toggle = {
        let (held, onchange) = (held.clone(), props.onchange.clone());
        move |name: GroupName, write: bool| {
            let (held, onchange) = (held.clone(), onchange.clone());
            Callback::from(move |checked: bool| {
                let mut next = held.clone();
                let entry = next.entry(name.to_string()).or_default();
                if write {
                    entry.write = checked;
                } else {
                    entry.read = checked;
                }
                onchange.emit(unfold(&next));
            })
        }
    };

    if props.groups.is_empty() {
        return html! {
            <p class="channel-picker__empty">
                { "This installation has no channels yet." }
            </p>
        };
    }

    html! {
        <ul class="channel-picker">
            { for props.groups.iter().map(|group| {
                let entry = held.get(group.name.as_str()).copied().unwrap_or_default();
                let locked = props.disabled || entry.claimed;
                let id = format!("{}-{}", props.id_prefix, group.bitpos);

                html! {
                    <li class="channel-picker__row" key={group.id.get()}>
                        <div class="channel-picker__identity">
                            <span class="channel-picker__name">{ group.name.to_string() }</span>
                            <span class="channel-picker__meta">
                                { group.source.label() }
                                if entry.claimed {
                                    { " · set by single sign-on" }
                                }
                            </span>
                        </div>

                        <Switch
                            id={format!("{id}-write")}
                            label="Write"
                            checked={entry.write}
                            disabled={locked}
                            onchange={toggle(group.name.clone(), true)}
                        />
                        <Switch
                            id={format!("{id}-read")}
                            label="Read"
                            checked={entry.read}
                            disabled={locked}
                            onchange={toggle(group.name.clone(), false)}
                        />
                    </li>
                }
            }) }
        </ul>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(name: &str, direction: Direction, source: MembershipSource) -> GroupMembership {
        GroupMembership {
            group: GroupName::from_storage(name),
            direction,
            source: Some(source),
        }
    }

    #[test]
    fn two_single_direction_rows_fold_into_one_channel() {
        let held = fold(&[
            grant("Blue", Direction::In, MembershipSource::Manual),
            grant("Blue", Direction::Out, MembershipSource::Manual),
        ]);

        assert_eq!(held.len(), 1);
        assert_eq!(held["Blue"].direction(), Some(Direction::Both));
    }

    #[test]
    fn a_membership_the_provider_owns_is_left_out_of_what_is_sent_back() {
        // Sending it would be refused, and accepting it would be accepting a
        // change that the member's next sign-in undoes.
        let held = fold(&[
            grant("Blue", Direction::Both, MembershipSource::Oidc),
            grant("Command", Direction::Out, MembershipSource::Manual),
        ]);

        let sent = unfold(&held);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].group.as_str(), "Command");
        assert_eq!(sent[0].direction, Direction::Out);
    }

    #[test]
    fn a_channel_with_neither_direction_is_not_a_membership() {
        let mut held = BTreeMap::new();
        held.insert("Blue".to_string(), Held::default());
        assert!(unfold(&held).is_empty());
    }
}
