//! `POST /api/v1/users/{username}/cloudtak-onboarding` and the one-shot
//! download it prepares.
//!
//! The flow itself is [`crate::identity::cloudtak`], which is also where the
//! argument for why this exception exists is written down. What is here is the
//! pair of routes and the four things they enforce at the edge:
//!
//! * both are `Administrative`, so an ordinary account cannot reach either,
//!   and the download is not a bearer link — it needs a session of its own;
//! * the download answers `410` for unknown, spent **and** expired identifiers
//!   alike, so it is not an oracle for which hand-overs are outstanding;
//! * the bytes are `application/x-pkcs12`, named after the account, and
//!   `Cache-Control: no-store` so a proxy does not keep a copy of a private
//!   key that this server has already deleted;
//! * the response body is never logged, and the two secrets on it redact
//!   themselves if something tries.

use actix_web::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use actix_web::{HttpResponse, web};
use rustak_api::CloudTakOnboardingRequest;

use crate::identity::cloudtak;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::subject::failed;

/// The media type a PKCS#12 keystore is served as.
const PKCS12: &str = "application/x-pkcs12";

/// What a caller is told when the bundle is not theirs to have, whatever the
/// reason. One sentence for three cases, deliberately.
const GONE: &str = "That keystore has already been downloaded or has expired. Onboard CloudTAK again to \
     prepare another one.";

/// Registers both routes.
///
/// The literal `cloudtak-onboarding` segment sits under `/users/{username}`,
/// where nothing else claims it, and the download has a scope of its own so
/// that the `{id}.p12` pattern is never weighed against a bare `{id}`.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route(
            "/users/{username}/cloudtak-onboarding",
            web::post().to(create),
        )
        .route("/cloudtak-onboarding/{id}.p12", web::get().to(download));
}

/// `POST /api/v1/users/{username}/cloudtak-onboarding`.
///
/// # Errors
///
/// A `400` for an unknown or disabled account, a credential that is not a live
/// client password for it, or a host that cannot go in a URL; a `403` for a
/// caller who does not administer the installation; a `503` when this
/// installation has no certificate authority yet; a `500` when issuing or
/// storing fails.
pub async fn create(
    context: web::Data<AppContext>,
    username: web::Path<String>,
    body: web::Json<CloudTakOnboardingRequest>,
    caller: Administrative,
) -> ApiResult {
    let username = Username::parse(&username.into_inner())
        .map_err(|err| ApiError::bad_request(err.to_string()))?;

    let user = context
        .db()
        .users()
        .get_by_username(&username)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::bad_request("There is no account with that username."))?;

    if user.disabled {
        return Err(ApiError::bad_request(
            "That account is disabled, so CloudTAK could not sign in as it.",
        ));
    }

    if !context.has_pki() {
        return Err(ApiError::new(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "This installation has no certificate authority yet, so there is nothing to issue a \
             certificate from.",
        ));
    }

    let prepared = cloudtak::onboard(
        &context,
        cloudtak::Onboarding {
            user: &user,
            actor: &caller.user.username,
            request: &body,
        },
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&prepared))
}

/// `GET /api/v1/cloudtak-onboarding/{id}.p12`.
///
/// # Errors
///
/// A `403` for a caller who does not administer the installation, a `410` for
/// an identifier that is unknown, already spent or expired, and a `500` when
/// the read or the decryption fails.
pub async fn download(
    context: web::Data<AppContext>,
    id: web::Path<String>,
    caller: Administrative,
) -> ApiResult {
    let bundle = cloudtak::take(&context, &id.into_inner())
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::gone(GONE))?;

    // After the take, never before: an entry for a download that did not happen
    // is worse than a missing one, and the take is what made it happen.
    if let Err(err) = cloudtak::record_download(&context, &bundle, &caller.user.username).await {
        warn!(error = %err, "Could not record a CloudTAK keystore download in the audit log.");
        context.session().record_human_error(&err);
    }

    let filename = format!("{}-cloudtak.p12", bundle.username);

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, PKCS12))
        .insert_header((
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ))
        // The server has already deleted its copy; a proxy holding one would
        // outlive the hand-over this whole feature is built to bound.
        .insert_header((CACHE_CONTROL, "no-store"))
        .body(bundle.p12))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::dev::{Path, ResourceDef};

    #[test]
    fn the_download_route_reads_the_identifier_before_the_extension() {
        // A literal suffix on a dynamic segment is the one piece of routing
        // here that is not obvious, and getting it wrong would mean the
        // identifier arriving with `.p12` still attached — which no stored
        // bundle would ever match.
        let resource = ResourceDef::new("/cloudtak-onboarding/{id}.p12");
        let mut path = Path::new("/cloudtak-onboarding/AbC-_123.p12");

        assert!(resource.capture_match_info(&mut path));
        assert_eq!(path.get("id"), Some("AbC-_123"));
    }

    #[test]
    fn a_path_with_no_extension_is_not_this_route() {
        let resource = ResourceDef::new("/cloudtak-onboarding/{id}.p12");
        let mut path = Path::new("/cloudtak-onboarding/AbC-_123");

        assert!(!resource.capture_match_info(&mut path));
    }

    #[test]
    fn the_refusal_names_no_reason_a_caller_could_learn_from() {
        // Unknown, spent and expired all answer this, so the endpoint cannot be
        // used to find out which hand-overs exist.
        assert!(!GONE.contains("expired,"), "{GONE}");
        assert!(GONE.contains("Onboard CloudTAK again"), "{GONE}");
    }
}
