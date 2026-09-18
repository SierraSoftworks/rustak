//! The server's own settings, as resolved from the configuration file and the
//! wizard together.

use rustak_api::ServerSettings;

use crate::api::{ApiError, get_json};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// The resolved settings. Values set in the configuration file win over the ones
/// the wizard wrote, so this is the only honest answer to "what is it actually
/// using".
pub async fn get() -> Result<ServerSettings, ApiError> {
    demo!(Ok(fixtures::server_settings()));

    get_json("/settings").await
}
