//! `POST /api/v1/config-packages`: the zip an operator sends to somebody
//! setting a client up by hand.
//!
//! The enrolment flow is the path we want people on — a one-time token, a
//! device-generated key, a certificate that never leaves the device. This is
//! the fallback for a client that cannot enrol, and it is deliberately narrow.
//!
//! # Nothing is minted here
//!
//! The request names a credential this installation already issued, and the
//! package is built from what is already stored. Building a credential as a
//! side effect of a download would put a long-lived secret in a file whose only
//! job is to be emailed around, and nothing would ever revoke it.
//!
//! # Why a keystore usually cannot be included
//!
//! rustak never holds a device's private key: a certificate is issued against
//! a signing request the device made, and the key stays there. There is
//! therefore nothing to assemble a `.p12` keystore out of after the fact, and
//! `include_client_cert` is refused with that explanation unless this
//! installation happens to hold a sealed key for one of the account's
//! certificates. The enrolment variant is the supported answer, and it is what
//! `include_client_cert: false` builds.

use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use actix_web::{HttpResponse, web};
use rustak_api::{
    AuditCategory, AuditOutcome, ConfigPackageRequest, ConfigPackageVariant, CredentialKind,
    Username,
};

use crate::db::AuditEntry;
use crate::prelude::*;
use crate::profiles::config_package::{ConfigPackageInput, build_itak, build_wintak_atak};
use crate::profiles::prefs::{APP_PREFS, UserSettings};

use super::error::{ApiError, ApiResult};
use super::extract::Administrative;
use super::subject::failed;

/// What the connection is called in a client's server list when the
/// installation has no better name for itself.
const DEFAULT_DESCRIPTION: &str = "rustak";

/// Registers the one route.
pub fn routes(config: &mut web::ServiceConfig) {
    config.route("/config-packages", web::post().to(create));
}

/// `POST /api/v1/config-packages`.
///
/// # Errors
///
/// A `400` for an unknown account, a credential that is not a live client
/// password for that account, or a request for a keystore we do not hold; a
/// `503` when this installation has no certificate authority yet; a `500` when
/// the package cannot be built.
pub async fn create(
    context: web::Data<AppContext>,
    body: web::Json<ConfigPackageRequest>,
    caller: Administrative,
) -> ApiResult {
    let request = body.into_inner();
    let user = account(&context, &request.username).await?;

    if let Some(id) = request.credential_id {
        check_credential(&context, id, user.id).await?;
    }

    let pki = context.pki().map_err(|err| {
        context.session().record_human_error(&err);

        ApiError::new(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "This installation has no certificate authority yet, so there is nothing to trust.",
        )
    })?;

    let chain = pki.chain();
    let links: Vec<&[u8]> = chain.iter().map(|link| link.as_ref()).collect();
    let options = pki.p12_options("rustak");

    let truststore =
        crate::pki::truststore(&links, &options).map_err(|err| failed(&context, &err))?;

    let config = context.config();
    let input = ConfigPackageInput {
        host: host(&config),
        stream_port: config.stream.tls.listen.port(),
        description: config
            .marti
            .public_host
            .clone()
            .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_string()),
        username: user.username.to_string(),
        truststore_p12: truststore,
        truststore_password: options.password.to_string(),
        client_p12: keystore(&request)?,
        app_prefs_group: APP_PREFS.to_string(),
        user: UserSettings {
            callsign: user.display_name.clone(),
            ..UserSettings::default()
        },
    };

    let (filename, body) = match request.variant {
        ConfigPackageVariant::WintakAtak => build_wintak_atak(&input),
        ConfigPackageVariant::Itak => build_itak(&input),
    }
    .map_err(|err| failed(&context, &err))?;

    record(&context, &caller, &request, &filename).await;

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, "application/zip"))
        .insert_header((
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(body))
}

/// The account, or a `400` naming what is wrong.
async fn account(
    context: &web::Data<AppContext>,
    username: &Username,
) -> Result<crate::db::repos::UserRow, ApiError> {
    let user = context
        .db()
        .users()
        .get_by_username(username)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::bad_request("There is no account with that username."))?;

    if user.disabled {
        return Err(ApiError::bad_request(
            "That account is disabled, so a package for it would not connect.",
        ));
    }

    Ok(user)
}

/// Refuses a credential that is not a live client password for this account.
async fn check_credential(
    context: &web::Data<AppContext>,
    id: rustak_api::CredentialId,
    user_id: rustak_api::UserId,
) -> Result<(), ApiError> {
    let credential = context
        .db()
        .credentials()
        .get(id)
        .await
        .map_err(|err| failed(context, &err))?
        .filter(|credential| credential.user_id == user_id)
        .ok_or_else(|| ApiError::bad_request("That account has no such credential."))?;

    if credential.kind != CredentialKind::ClientPassword {
        return Err(ApiError::bad_request(
            "A configuration package is enrolled with a client password, not with that kind of credential.",
        ));
    }

    if credential.revoked_at.is_some()
        || credential
            .expires_at
            .is_some_and(|at| at <= chrono::Utc::now())
    {
        return Err(ApiError::bad_request(
            "That credential has expired or been revoked; issue another one first.",
        ));
    }

    Ok(())
}

/// The account's own keystore, which this server does not hold.
///
/// Kept as a function rather than inlined so that the reason is stated once,
/// beside the refusal a caller reads.
fn keystore(request: &ConfigPackageRequest) -> Result<Option<(Vec<u8>, String)>, ApiError> {
    if !request.include_client_cert {
        return Ok(None);
    }

    Err(ApiError::bad_request(
        "This server does not hold a device's private key, so it cannot build a keystore for one. \
         Build the package without a client certificate: the client will enrol for its own on first connect.",
    ))
}

/// The host a client should connect to.
fn host(config: &Config) -> String {
    config
        .marti
        .public_host
        .clone()
        .or_else(|| config.server.canonical_domain().map(str::to_string))
        .unwrap_or_else(|| "localhost".to_string())
}

/// Writes who built a package for whom.
async fn record(
    context: &AppContext,
    caller: &Administrative,
    request: &ConfigPackageRequest,
    filename: &str,
) {
    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "config_package.built",
        AuditOutcome::Success,
    )
    .subject(request.username.as_str())
    .actor(&caller.user.username)
    .detail(serde_json::json!({
        "variant": request.variant.as_str(),
        "filename": filename,
        "with_client_certificate": request.include_client_cert,
    }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a configuration package in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keystore_is_refused_in_words_rather_than_minted() {
        let request = ConfigPackageRequest {
            username: Username::parse("ada").unwrap(),
            credential_id: None,
            variant: ConfigPackageVariant::WintakAtak,
            include_client_cert: true,
        };

        let err = keystore(&request).unwrap_err();
        assert_eq!(err.status(), actix_web::http::StatusCode::BAD_REQUEST);

        assert_eq!(
            keystore(&ConfigPackageRequest {
                include_client_cert: false,
                ..request
            })
            .unwrap(),
            None,
        );
    }
}
