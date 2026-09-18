//! Everybody who can sign in.

use rustak_api::{User, UserPatch, Username};

use crate::api::{ApiError, get_json, patch_json};
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
