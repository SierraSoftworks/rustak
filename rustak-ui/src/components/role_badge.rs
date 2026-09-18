//! What one subscriber may do to a mission, said in one word.
//!
//! The wire spellings (`MISSION_OWNER`, `MISSION_READONLY_SUBSCRIBER`) are
//! TAK's and have to travel unchanged, but they are not what anybody reads —
//! so the label and the wire value are separated here, once, rather than in
//! each page that shows a role.
//!
//! The tone is not decoration either. "Read-only" is the one role that cannot
//! write, and an operator scanning a subscriber list for *who can change this
//! mission* is asking exactly that question.

use rustak_api::MissionRoleKind;
use yew::prelude::*;

use crate::components::{SelectOption, StatusPill, StatusTone};

/// The label a person reads for a role.
pub fn role_label(role: MissionRoleKind) -> &'static str {
    match role {
        MissionRoleKind::Owner => "Owner",
        MissionRoleKind::Subscriber => "Subscriber",
        MissionRoleKind::ReadonlySubscriber => "Read-only",
    }
}

/// What the role means, for the title attribute.
pub fn role_description(role: MissionRoleKind) -> &'static str {
    match role {
        MissionRoleKind::Owner => {
            "May change the mission itself, invite others and set their roles."
        }
        MissionRoleKind::Subscriber => "May add and remove content.",
        MissionRoleKind::ReadonlySubscriber => {
            "Receives everything and may change nothing. Anything it sends is dropped."
        }
    }
}

/// The tone a role is shown in: the one that cannot write stands apart.
fn tone(role: MissionRoleKind) -> StatusTone {
    match role {
        MissionRoleKind::Owner => StatusTone::Ok,
        MissionRoleKind::Subscriber => StatusTone::Neutral,
        MissionRoleKind::ReadonlySubscriber => StatusTone::Warning,
    }
}

/// Every role as a `Select`'s options, in the order the API lists them.
pub fn role_options() -> Vec<SelectOption> {
    MissionRoleKind::ALL
        .iter()
        .map(|role| SelectOption::new(role.as_str(), role_label(*role)))
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct RoleBadgeProps {
    pub role: MissionRoleKind,
}

/// One subscriber's role, as a pill.
#[function_component(RoleBadge)]
pub fn role_badge(props: &RoleBadgeProps) -> Html {
    html! {
        <StatusPill
            tone={tone(props.role)}
            label={role_label(props.role)}
            title={role_description(props.role)}
        />
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_a_label_and_an_option_that_carries_the_wire_spelling() {
        let options = role_options();

        assert_eq!(options.len(), MissionRoleKind::ALL.len());

        for (role, option) in MissionRoleKind::ALL.iter().zip(options.iter()) {
            assert_eq!(
                option.value.as_str(),
                role.as_str(),
                "the option's value is what goes back on the wire",
            );
            assert_eq!(option.label.as_str(), role_label(*role));
            assert!(!role_description(*role).is_empty());
        }
    }

    #[test]
    fn a_role_option_round_trips_back_into_a_role() {
        for option in role_options() {
            assert!(
                MissionRoleKind::parse(option.value.as_str()).is_some(),
                "a chosen option has to parse back: {}",
                option.value,
            );
        }
    }

    #[test]
    fn only_the_read_only_role_is_marked_as_the_one_that_cannot_write() {
        for role in MissionRoleKind::ALL.iter().copied() {
            assert_eq!(
                tone(role) == StatusTone::Warning,
                !role.can_write(),
                "{role:?} should stand apart exactly when it cannot write",
            );
        }
    }
}
