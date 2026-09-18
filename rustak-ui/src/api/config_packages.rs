//! The manual configuration package.
//!
//! An enrolling device fetches everything it needs over HTTPS; this is the
//! other path — a zip an operator sends out of band, which somebody imports
//! into ATAK, WinTAK or iTAK to end up with the same stream configured.
//!
//! # Nothing is minted here
//!
//! The request names a credential that already exists and the server packages
//! what is already stored. Building one as a side effect of a download would
//! put a long-lived secret into a file whose only job is to be emailed around,
//! and nothing would ever revoke it — so the credential picker offers what is
//! there and nothing else.

use rustak_api::ConfigPackageRequest;

use crate::api::ApiError;
use crate::api::download::{Download, post_download};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// Builds a package for one account, and hands it back to be saved.
pub async fn create(request: &ConfigPackageRequest) -> Result<Download, ApiError> {
    demo!(fixtures::config_package(request));

    post_download("/config-packages", request, "config.zip").await
}
