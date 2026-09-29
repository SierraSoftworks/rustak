//! What the sign-in rate limiter is refusing, and forgiving one key of it.
//!
//! Administrators only; anybody else is answered `403`.

use rustak_api::{ClearLockoutRequest, Lockout, Lockouts};

use crate::api::{ApiError, get_json, post_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// Every key locked out now, newest first, and the counts per class.
pub async fn list() -> Result<Lockouts, ApiError> {
    demo!(Ok(fixtures::lockouts()));

    get_json("/auth/lockouts").await
}

/// Forgives one lockout. Answers the lockout that was cleared.
pub async fn clear(lockout: &Lockout) -> Result<Lockout, ApiError> {
    let request = ClearLockoutRequest {
        class: lockout.class,
        address: lockout.address,
        key: lockout.key.clone(),
    };

    demo!(fixtures::clear_lockout(&request));

    post_json("/auth/lockouts/clear", &request).await
}
