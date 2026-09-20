//! The sidecars registered with this server.
//!
//! `GET /services` is administrative — the listing names every plugin in the
//! installation, which is not a sidecar's business — while the per-service
//! routes are shared with the sidecars themselves: a service reads only its
//! own configuration, and only an administrator writes one. The console is
//! always the administrator, so every call here goes out with the session's
//! bearer token like any other admin request.
//!
//! The name in the path is the service's own `name`, not its row id, because
//! that is what the control API is keyed on — a sidecar that restarts keeps
//! its name and gets a new row.

use rustak_api::ServiceSummary;

use crate::api::{ApiError, delete_empty, get_json, put_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every registered service, newest registration last.
pub async fn list() -> Result<Vec<ServiceSummary>, ApiError> {
    demo!(Ok(fixtures::services()));

    get_json("/services").await
}

/// The configuration an administrator has set for one service.
///
/// An object, always — the server refuses a configuration that is not one — and
/// `{}` for a service nobody has configured.
pub async fn config(name: &str) -> Result<serde_json::Value, ApiError> {
    demo!(fixtures::service_config(name));

    get_json(&format!("/services/{}/config", urlencode(name))).await
}

/// Replaces a service's configuration, answering with what was stored.
///
/// A replacement rather than a merge, which is what makes "this is the whole
/// setting" one request. The service reads it on its next tick; nothing here
/// pushes it.
pub async fn set_config(
    name: &str,
    config: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    demo!(fixtures::set_service_config(name, config));

    put_json(&format!("/services/{}/config", urlencode(name)), config).await
}

/// Removes a registration.
///
/// Not a cascade: the account the sidecar authenticates as, and the certificate
/// it holds, are untouched — so a plugin that is merely stopped can be removed
/// from the listing and register again when it comes back.
pub async fn remove(name: &str) -> Result<(), ApiError> {
    demo!(fixtures::remove_service(name));

    delete_empty(&format!("/services/{}", urlencode(name))).await
}
