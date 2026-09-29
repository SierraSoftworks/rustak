//! Adding a service: the account, its enrolment token and its service token in
//! one administrative call.
//!
//! The response is the one place either secret exists outside the deployment
//! that will use it — the server keeps argon2 hashes — so nothing here caches
//! it, and the page that shows it drops it when it is dismissed.

use rustak_api::{ServiceOnboarding, ServiceOnboardingRequest};

use crate::api::{ApiError, post_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// Sets up one service, or refreshes its enrolment token if it is already set
/// up.
pub async fn onboard(request: &ServiceOnboardingRequest) -> Result<ServiceOnboarding, ApiError> {
    demo!(fixtures::service_onboarding(request));

    post_json("/service-onboarding", request).await
}
