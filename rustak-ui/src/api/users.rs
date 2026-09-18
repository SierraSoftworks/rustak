//! Everybody who can sign in.

use rustak_api::{CreateUserRequest, User, UserPatch, Username};

use crate::api::{ApiError, get_json, patch_json, post_json};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every user in the installation.
pub async fn list() -> Result<Vec<User>, ApiError> {
    demo!(Ok(fixtures::users()));

    get_json("/users").await
}

/// Promotes, demotes, renames or suspends a user.
pub async fn patch(username: &Username, patch: &UserPatch) -> Result<User, ApiError> {
    demo!(
        fixtures::patch_user(username, patch)
            .ok_or(ApiError::Server("That user no longer exists.".to_string()))
    );

    patch_json(&format!("/users/{}", urlencode(username.as_str())), patch).await
}

/// Creates an account.
///
/// There is no credential here and no way to add one: rustak has no local
/// passwords, so a new account signs in with a passkey, arrives through the
/// identity provider, or is given a credential of its own afterwards.
pub async fn create(request: &CreateUserRequest) -> Result<User, ApiError> {
    demo!(fixtures::create_user(request));

    post_json("/users", request).await
}

/// One account by name, read out of the listing.
///
/// The server has no `GET /users/{username}`, and a page that opens on one
/// account still has to know whether that account is there — so the list is the
/// read, and the filter is here rather than in every page that needs it.
pub async fn get(username: &Username) -> Result<User, ApiError> {
    let wanted = username.clone();
    list()
        .await?
        .into_iter()
        .find(|user| user.username == wanted)
        .ok_or_else(|| ApiError::Server("There is no account by that name.".to_string()))
}
