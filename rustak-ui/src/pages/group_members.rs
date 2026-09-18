//! Who is in one channel.
//!
//! There is no `GET /api/v1/groups/{name}/members`: membership is stored and
//! written per account, and the only endpoint that reads it is
//! `GET /api/v1/users/{username}/groups`. So this composes the table from the
//! account list and one read per account, and a toggle writes back that one
//! account's whole set — which is what `PUT` expects, and what keeps the change
//! idempotent rather than a grant that could half-apply.

use futures::future::join_all;
use rustak_api::{Direction, Group, GroupMembership, User, Username};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::api::ApiError;
use crate::components::{Alert, AlertKind, Card, LoadingNote, Switch};

use super::load::use_resource;

/// Every account with what it holds, which is the whole table in one value.
type Roster = Vec<(User, Vec<GroupMembership>)>;

async fn roster() -> Result<Roster, ApiError> {
    let users = api::users::list().await?;

    let held = join_all(
        users
            .iter()
            .map(|user| api::groups::memberships(&user.username)),
    )
    .await;

    users
        .into_iter()
        .zip(held)
        .map(|(user, held)| held.map(|held| (user, held)))
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct GroupMembersProps {
    pub group: Group,
}

#[function_component(GroupMembers)]
pub fn group_members(props: &GroupMembersProps) -> Html {
    let roster = use_resource(roster);

    let body = match (&roster.data, &roster.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the members."
                message={message.clone()}
            />
        },
        (Some(rows), _) => html! {
            <ul class="member-list">
                { for rows.iter().map(|(user, held)| html! {
                    <li key={user.id.get()}>
                        <MemberRow
                            user={user.clone()}
                            held={held.clone()}
                            group={props.group.name.to_string()}
                            on_changed={roster.reload.clone()}
                        />
                    </li>
                }) }
            </ul>
        },
    };

    html! {
        <Card
            title={format!("Members of {}", props.group.name)}
            subtitle="Write is permission to send into the channel; read is permission to \
                receive from it."
        >
            { body }
        </Card>
    }
}

#[derive(Properties, PartialEq)]
struct MemberRowProps {
    user: User,
    held: Vec<GroupMembership>,
    group: String,
    on_changed: Callback<()>,
}

#[function_component(MemberRow)]
fn member_row(props: &MemberRowProps) -> Html {
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let mine: Vec<&GroupMembership> = props
        .held
        .iter()
        .filter(|grant| grant.group.as_str() == props.group)
        .collect();

    let write = mine
        .iter()
        .any(|grant| grant.direction.includes(Direction::In));
    let read = mine
        .iter()
        .any(|grant| grant.direction.includes(Direction::Out));
    // A membership the identity provider owns is rewritten at the member's next
    // sign-in, so changing it here would last until then and no longer — and the
    // server refuses the attempt outright.
    let claimed = mine
        .iter()
        .any(|grant| grant.source == Some(rustak_api::MembershipSource::Oidc));

    let toggle = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let (username, group, held) = (
            props.user.username.clone(),
            props.group.clone(),
            props.held.clone(),
        );

        move |direction_is_write: bool, currently: bool| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let (username, group, held) = (username.clone(), group.clone(), held.clone());

            Callback::from(move |_: bool| {
                let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
                let username: Username = username.clone();
                let wanted = rewrite(&held, &group, direction_is_write, !currently, (write, read));

                busy.set(true);
                spawn_local(async move {
                    match api::groups::set_memberships(&username, &wanted).await {
                        Ok(_) => {
                            error.set(None);
                            on_changed.emit(());
                        }
                        Err(err) => error.set(Some(err.to_string())),
                    }
                    busy.set(false);
                });
            })
        }
    };

    let id = format!("member-{}-{}", props.user.id.get(), props.group);

    html! {
        <div class="member-row">
            <div class="member-row__identity">
                <span class="member-row__name">{ props.user.display().to_string() }</span>
                <span class="member-row__username">{ props.user.username.to_string() }</span>
            </div>

            <Switch
                id={format!("{id}-write")}
                label="Write"
                checked={write}
                disabled={*busy || claimed}
                onchange={toggle(true, write)}
            />
            <Switch
                id={format!("{id}-read")}
                label="Read"
                checked={read}
                disabled={*busy || claimed}
                onchange={toggle(false, read)}
            />

            if claimed {
                <span class="member-row__note">{ "Set by single sign-on" }</span>
            }

            if let Some(message) = &*error {
                <p class="member-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

/// The whole set to send back, with one channel's one direction changed.
///
/// `PUT` replaces the manual grants wholesale, so every other channel this
/// account holds has to be carried along — dropping one here would silently
/// revoke it.
fn rewrite(
    held: &[GroupMembership],
    group: &str,
    write_column: bool,
    checked: bool,
    current: (bool, bool),
) -> Vec<GroupMembership> {
    let (write, read) = current;
    let (write, read) = if write_column {
        (checked, read)
    } else {
        (write, checked)
    };

    let mut next: Vec<GroupMembership> = held
        .iter()
        .filter(|grant| grant.group.as_str() != group)
        .filter(|grant| grant.source != Some(rustak_api::MembershipSource::Oidc))
        .cloned()
        .map(|grant| GroupMembership {
            source: None,
            ..grant
        })
        .collect();

    if let Some(direction) = match (write, read) {
        (true, true) => Some(Direction::Both),
        (true, false) => Some(Direction::In),
        (false, true) => Some(Direction::Out),
        (false, false) => None,
    } {
        next.push(GroupMembership::new(
            rustak_api::GroupName::from_storage(group),
            direction,
        ));
    }

    next
}

#[cfg(test)]
mod tests {
    use rustak_api::GroupName;

    use super::*;

    fn grant(name: &str, direction: Direction) -> GroupMembership {
        GroupMembership::new(GroupName::from_storage(name), direction)
    }

    #[test]
    fn switching_one_channel_off_leaves_the_others_alone() {
        let held = vec![
            grant("Blue", Direction::Both),
            grant("Command", Direction::Out),
        ];

        let next = rewrite(&held, "Blue", true, false, (true, true));

        assert_eq!(next.len(), 2);
        assert!(next.iter().any(|g| g.group.as_str() == "Command"));
        let blue = next.iter().find(|g| g.group.as_str() == "Blue").unwrap();
        assert_eq!(blue.direction, Direction::Out);
    }

    #[test]
    fn switching_the_last_direction_off_removes_the_membership() {
        let held = vec![grant("Blue", Direction::Out)];

        let next = rewrite(&held, "Blue", false, false, (false, true));

        assert!(next.is_empty());
    }

    #[test]
    fn a_channel_not_held_at_all_is_added() {
        let next = rewrite(&[], "Logistics", false, true, (false, false));

        assert_eq!(next.len(), 1);
        assert_eq!(next[0].direction, Direction::Out);
    }
}
