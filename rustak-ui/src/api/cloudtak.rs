//! Onboarding CloudTAK.
//!
//! Two calls that belong together: one prepares everything CloudTAK's server
//! setup asks for, and the other collects the keystore it prepared — once.
//!
//! # The download is not a link
//!
//! It is behind the bearer token the application holds in `sessionStorage`,
//! like every other download here, so it is fetched and then saved rather than
//! navigated to. That matters more here than for a mission archive: the server
//! deletes its copy as it answers, so an anchor that silently failed would
//! consume the hand-over and leave nothing to show for it.
//!
//! The server returns the download as an absolute path under `/api/v1`, since
//! that is what it would give any client; [`p12`] trims the prefix back off
//! because [`crate::api`] adds it to everything it sends.

use rustak_api::{CloudTakOnboarding, CloudTakOnboardingRequest, Username};

use crate::api::download::{Download, get_download};
use crate::api::{API_BASE, ApiError, post_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Prepares a hand-over for one account.
pub async fn onboard(
    username: &Username,
    request: &CloudTakOnboardingRequest,
) -> Result<CloudTakOnboarding, ApiError> {
    demo!(fixtures::cloudtak_onboarding(username, request));

    post_json(
        &format!(
            "/users/{}/cloudtak-onboarding",
            urlencode(username.as_str())
        ),
        request,
    )
    .await
}

/// What a `410` means here, as opposed to what it means anywhere else.
///
/// [`ApiError::Gone`] deliberately has no message of its own — the things a
/// `410` means on this server have nothing in common to say — so the sentence
/// is supplied by the one caller that knows which question was asked. It is
/// fitted here rather than in the page so that the page has one error path.
const GONE: &str = "That keystore has already been downloaded, or the ten minutes ran out. \
                    Dismiss this and onboard CloudTAK again to prepare another one.";

/// Collects the keystore a hand-over prepared, which works exactly once.
///
/// `url` is [`CloudTakOnboarding::p12_download_url`] exactly as the server gave
/// it.
pub async fn p12(url: &str, username: &Username) -> Result<Download, ApiError> {
    demo!(fixtures::cloudtak_p12(url, username).map_err(gone));

    let path = url.strip_prefix(API_BASE).unwrap_or(url);

    get_download(path, &format!("{username}-cloudtak.p12"))
        .await
        .map_err(gone)
}

/// Replaces the generic `410` with the one this endpoint means.
fn gone(err: ApiError) -> ApiError {
    match err {
        ApiError::Gone => ApiError::Server(GONE.to_string()),
        other => other,
    }
}
