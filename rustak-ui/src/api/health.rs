//! Whether the server is well.

use rustak_api::Health;

use crate::api::{ApiError, get_json};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// The server's health. Public, so the dashboard can say something useful even
/// when the session has gone stale.
pub async fn get() -> Result<Health, ApiError> {
    demo!(Ok(fixtures::health()));

    get_json("/health").await
}
