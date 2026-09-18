//! The first-run wizard's routes.
//!
//! Every one of them answers `410 Gone` once the wizard has been completed,
//! which arrives here as [`ApiError::Gone`] — so a wizard left open in a tab
//! says so rather than appearing to work.

use rustak_api::{
    AdminCreated, CaSummary, CreateAdminRequest, InitCaRequest, ServerSettings,
    ServerSettingsRequest, SetupStatus,
};

use crate::api::{ApiError, get_json, post_empty, post_json};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// How far through the wizard this installation is.
///
/// Public: the UI has to ask before anybody can sign in, so that a fresh server
/// sends people to the wizard rather than to a login page they cannot use.
pub async fn status() -> Result<SetupStatus, ApiError> {
    demo!(Ok(fixtures::setup_status()));

    get_json("/setup/status").await
}

/// Creates the first administrator, gated by the one-time setup token the server
/// wrote out at first start.
pub async fn create_admin(request: &CreateAdminRequest) -> Result<AdminCreated, ApiError> {
    demo!(fixtures::create_admin(request));

    post_json("/setup/admin", request).await
}

/// Tells the server what it is called and where it is reachable.
pub async fn set_server(request: &ServerSettingsRequest) -> Result<ServerSettings, ApiError> {
    demo!(Ok(fixtures::set_server_settings(request)));

    post_json("/setup/server", request).await
}

/// Creates the internal certificate authority.
pub async fn init_ca(request: &InitCaRequest) -> Result<CaSummary, ApiError> {
    demo!(Ok(fixtures::init_ca(request)));

    post_json("/setup/ca", request).await
}

/// Stamps the wizard as finished, after which every route above is gone.
pub async fn complete() -> Result<(), ApiError> {
    demo!(fixtures::complete_setup(); Ok(()));

    post_empty("/setup/complete", &serde_json::json!({})).await
}
