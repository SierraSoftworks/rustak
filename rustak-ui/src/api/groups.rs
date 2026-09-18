//! Channels, which TAK calls groups, and who is in them.
//!
//! Administrative throughout: a channel is a routing decision that applies to
//! everybody holding it rather than a document somebody owns, so the server
//! refuses every call here to anyone who is not an administrator.

use rustak_api::{CreateGroupRequest, Group, GroupMembership, GroupName, GroupPatch, Username};

use crate::api::{ApiError, delete_empty, get_json, patch_json, post_json, put_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every channel this installation has.
pub async fn list() -> Result<Vec<Group>, ApiError> {
    demo!(Ok(fixtures::groups()));

    get_json("/groups").await
}

/// Creates one. The bit position is the server's to allocate.
pub async fn create(request: &CreateGroupRequest) -> Result<Group, ApiError> {
    demo!(fixtures::create_group(request));

    post_json("/groups", request).await
}

/// Changes a channel's description.
///
/// Not its name: every membership, every `groups` claim and every client's
/// cached selection refers to a channel by name, so renaming one would be
/// deleting it and making another.
pub async fn patch(name: &GroupName, change: &GroupPatch) -> Result<Group, ApiError> {
    demo!(fixtures::patch_group(name, change));

    patch_json(&format!("/groups/{}", urlencode(name.as_str())), change).await
}

/// Removes one. The bit position stays reserved, because a live subscription
/// holds positions rather than names.
pub async fn remove(name: &GroupName) -> Result<(), ApiError> {
    demo!(fixtures::delete_group(name));

    delete_empty(&format!("/groups/{}", urlencode(name.as_str()))).await
}

/// What one account may read from and write into.
pub async fn memberships(username: &Username) -> Result<Vec<GroupMembership>, ApiError> {
    demo!(Ok(fixtures::memberships_of(username)));

    get_json(&format!("/users/{}/groups", urlencode(username.as_str()))).await
}

/// Replaces the grants an administrator made, leaving the ones the identity
/// provider owns alone — they are rewritten wholesale at the member's next
/// sign-in, so a change made here would not last.
pub async fn set_memberships(
    username: &Username,
    wanted: &[GroupMembership],
) -> Result<Vec<GroupMembership>, ApiError> {
    demo!(fixtures::set_memberships(username, wanted));

    put_json(
        &format!("/users/{}/groups", urlencode(username.as_str())),
        &wanted.to_vec(),
    )
    .await
}
