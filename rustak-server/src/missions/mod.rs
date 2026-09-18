//! Data Sync: missions, what is filed under them, and who may see it.
//!
//! A *mission* is what TAK Server calls a shared, server-persisted workspace
//! and what CloudTAK calls a **Data Sync**: a named collection of map items and
//! files, an access rule, and an append-only log of what changed. The route
//! files under [`crate::marti::missions`] parse; everything here decides.
//!
//! # The shape of this module
//!
//! * [`model`] — the mission itself and the parameter structs a route builds.
//! * [`roles`] — the three roles, the eight permissions, and the check.
//! * [`service`] — [`MissionService`]: resolve, list, create, update, delete,
//!   copy and the mission tree.
//! * [`contents`] — filing map items and resources, and rendering a mission.
//! * [`import`] — the same, from a Mission Package zip.
//! * [`changes`] — the change log, the squash, and the CoT view.
//! * [`subscriptions`] — who is watching, what role they hold, and the tokens.
//! * [`keywords`] — replacing the tags on a mission, an item or a resource.
//! * [`dto`] — the exact JSON all of the above is rendered as.
//!
//! # Mission passwords are argon2id, not bcrypt
//!
//! TAK Server hashes a mission password with bcrypt at cost 10, and rustak does
//! not follow it. Nothing in the wire contract exposes the hash: a client sends
//! the password, we answer with an `ACCESS` token we minted ourselves, and the
//! stored hash is never transmitted, federated or compared by anybody else. So
//! there is no parity to keep, and the rest of rustak already hashes every
//! verifiable secret with argon2id at one set of parameters — a second
//! algorithm would be a second thing to review.
//!
//! # The rest of the module
//!
//! * [`invitations`] — who may join a mission they cannot otherwise see.
//! * [`logs`] — the written record beside the map.
//! * [`layers`] — the folder tree contents are filed under.
//! * [`external`] — map layers, external data and data feeds.
//! * [`archive`] — a mission as a Mission Package, and the delete's undo.
//! * [`cot`] — `<dest mission>`: the stream's way into a Data Sync.
//! * [`notify`] — turning a write into the `t-x-m-*` its watchers get.

pub mod archive;
pub mod changes;
pub mod contents;
pub mod cot;
pub mod crud;
pub mod dto;
pub mod external;
pub mod import;
pub mod invitations;
pub mod invitees;
pub mod keywords;
pub mod layers;
pub mod logs;
pub mod model;
pub mod notify;
pub mod render;
pub mod roles;
pub mod service;
pub mod subscriptions;

use chrono::{DateTime, Utc};

use crate::marti::time;

pub use cot::MissionPublisher;
pub use dto::{
    MissionAddJson, MissionChangeJson, MissionContentBody, MissionJson, MissionRoleJson,
    MissionSubscriptionJson, UidDetailsJson,
};
pub use model::{CopyParams, KeywordTarget, Mission, MissionParams, Outcome, validate_name};
pub use notify::notice_mission;
pub use render::Render;
pub use roles::{MissionRole, Permission, Role};
pub use service::{ListFilter, MissionService};
pub use subscriptions::{MissionSubscription, SubscribeReq};

/// Renders a role as the `{type, permissions}` object every client reads.
pub fn role_json(role: Role) -> MissionRoleJson {
    MissionRoleJson {
        kind: role.as_str().to_string(),
        permissions: role
            .permissions()
            .iter()
            .map(|permission| permission.as_str().to_string())
            .collect(),
    }
}

/// Renders a subscription as the wire shape.
///
/// `createTime` is the **unpadded** millisecond form here — `Mission` uses the
/// padded one, and a client reads whichever the object it is holding carries.
pub fn subscription_json(
    subscription: &MissionSubscription,
    mission: Option<MissionJson>,
    include_token: bool,
) -> MissionSubscriptionJson {
    MissionSubscriptionJson {
        token: include_token.then(|| subscription.token.clone()).flatten(),
        mission,
        client_uid: subscription.client_uid.clone(),
        username: subscription.username.clone().unwrap_or_default(),
        create_time: unpadded(subscription.create_time),
        role: role_json(subscription.role),
    }
}

/// The unpadded-millisecond date form the subscription object uses.
fn unpadded(at: DateTime<Utc>) -> String {
    time::cot_date_unpadded(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscription_omits_its_token_when_the_listing_says_to() {
        let subscription = MissionSubscription {
            client_uid: "ANDROID-1".to_string(),
            username: None,
            role: Role::Subscriber,
            create_time: DateTime::UNIX_EPOCH,
            token: Some("t".to_string()),
        };

        assert_eq!(
            subscription_json(&subscription, None, false).token,
            None,
            "the plural role listing never carries tokens"
        );
        assert_eq!(
            subscription_json(&subscription, None, true)
                .token
                .as_deref(),
            Some("t")
        );
        assert_eq!(subscription_json(&subscription, None, true).username, "");
    }

    #[test]
    fn an_owner_role_renders_all_eight_permissions() {
        assert_eq!(role_json(Role::Owner).permissions.len(), 8);
        assert_eq!(role_json(Role::Owner).kind, "MISSION_OWNER");
    }
}
