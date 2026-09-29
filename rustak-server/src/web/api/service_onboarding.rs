//! `POST /api/v1/service-onboarding`: add a service in one step.
//!
//! The flow is [`crate::identity::service_onboarding`]; this is the route and
//! the three things it enforces at the edge:
//!
//! * it is `Administrative`, so an ordinary account — and a sidecar's own
//!   service token, which the session gate does not accept — cannot reach it;
//! * a refusal is a `400` when the name cannot be a service's and a `409` when
//!   it belongs to somebody else, so a caller can tell a typo from a clash;
//! * the response carries two secrets and is never logged; the type redacts
//!   them if something tries.
//!
//! It sits on a path of its own rather than under `/services`, because that
//! prefix is the control API: mounted outside the session gate and answering
//! sidecars, where an administrative action has no business being matched.
//!
//! Like the other credential-minting actions (`POST /credentials`, the CloudTAK
//! hand-over) it is not rate limited on its own: it needs an administrator's
//! session, and the routes that issue one are.

use actix_web::web;
use rustak_api::ServiceOnboardingRequest;

use crate::identity::service_onboarding::{self, Refusal};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::subject::failed;

/// Registers the route.
pub fn routes(config: &mut web::ServiceConfig) {
    config.route("/service-onboarding", web::post().to(create));
}

/// `POST /api/v1/service-onboarding`.
///
/// # Errors
///
/// A `400` for a name that cannot be a service's or an account's, a `403` for a
/// caller who does not administer the installation, a `409` for a person's
/// account, a switched-off service account or a name registered to another
/// account, and a `500` when storage fails — after undoing what was written.
pub async fn create(
    context: web::Data<AppContext>,
    body: web::Json<ServiceOnboardingRequest>,
    caller: Administrative,
) -> ApiResult {
    match service_onboarding::onboard(&context, &body, &caller.user.username).await {
        Ok(onboarding) => Ok(json_ok(&onboarding)),
        Err(Refusal::Invalid(message)) => Err(ApiError::bad_request(message)),
        Err(Refusal::Conflict(message)) => Err(ApiError::conflict(message)),
        Err(Refusal::Failed(err)) => Err(failed(&context, &err)),
    }
}
