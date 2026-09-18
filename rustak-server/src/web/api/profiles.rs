//! `/api/v1/profiles`: the device profiles an administrator edits.
//!
//! Administrative throughout. A profile decides what every device that matches
//! its channels is configured with, so editing one is an operator's job and
//! nothing here is reachable without a session.
//!
//! # Preferences are validated, not just stored
//!
//! A value that does not fit the class it was sent with — `Boolean` holding
//! `"yes"`, `Integer` holding `"3.5"` — is refused here rather than delivered.
//! ATAK would import it and then behave oddly, which is a far harder failure to
//! find than a `400` in the editor.

use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use actix_web::{HttpResponse, web};
use rustak_api::{AuditCategory, AuditOutcome, PrefEntry, ProfileCreate, ProfileId, ProfileUpdate};

use crate::db::AuditEntry;
use crate::prelude::*;
use crate::profiles::builder::{PROFILE_FILENAME, build_profile_package};
use crate::profiles::catalog;
use crate::profiles::model::{NewProfile, ProfileRow};
use crate::profiles::repo::ProfilesRepo;
use crate::profiles::service::ProfileService;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::profile_files as files;
use super::subject::failed;

/// Registers every profile route, catalogue first so that the literal segment
/// is not swallowed by `{id}`.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/profiles/pref-catalog", web::get().to(pref_catalog))
        .route("/profiles", web::get().to(list))
        .route("/profiles", web::post().to(create))
        .route("/profiles/{id}", web::get().to(get))
        .route("/profiles/{id}", web::patch().to(patch))
        .route("/profiles/{id}", web::delete().to(remove))
        .route("/profiles/{id}/files", web::get().to(files::list))
        .route("/profiles/{id}/files", web::post().to(files::upload))
        .route(
            "/profiles/{id}/files/{file}",
            web::get().to(files::download),
        )
        .route(
            "/profiles/{id}/files/{file}",
            web::delete().to(files::remove),
        )
        .route("/profiles/{id}/prefs", web::get().to(get_prefs))
        .route("/profiles/{id}/prefs", web::put().to(put_prefs))
        .route("/profiles/{id}/preview", web::get().to(preview));
}

/// `GET /api/v1/profiles`.
///
/// # Errors
///
/// A `500` when a read fails.
pub async fn list(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let listed = service(&context)?
        .list()
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&listed))
}

/// `POST /api/v1/profiles`.
///
/// # Errors
///
/// A `400` for a blank or duplicate name, and a `500` when a write fails.
pub async fn create(
    context: web::Data<AppContext>,
    body: web::Json<ProfileCreate>,
    caller: Administrative,
) -> ApiResult {
    let request = body.into_inner();
    let name = request.name.trim().to_string();

    if name.is_empty() {
        return Err(ApiError::bad_request("A profile needs a name."));
    }

    let created = ProfilesRepo::new(context.db())
        .create(NewProfile {
            name: name.clone(),
            description: request.description,
            active: request.active.unwrap_or(true),
            apply_on_enrollment: request.apply_on_enrollment,
            apply_on_connect: request.apply_on_connect,
            tool: request.tool,
            kind: request.kind,
            groups: request.groups,
        })
        .await
        .map_err(|err| failed(&context, &err))?;

    record(&context, "profile.created", &caller, &name).await;

    described(&context, created).await
}

/// `GET /api/v1/profiles/{id}`.
///
/// # Errors
///
/// A `404` when there is no such profile, and a `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    _: Administrative,
) -> ApiResult {
    let row = row(&context, *id).await?;

    described(&context, row).await
}

/// `PATCH /api/v1/profiles/{id}`.
///
/// # Errors
///
/// A `400` for a change that would do nothing, a `404`, and a `500`.
pub async fn patch(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    body: web::Json<ProfileUpdate>,
    caller: Administrative,
) -> ApiResult {
    let change = body.into_inner();

    if change.is_empty() {
        return Err(ApiError::bad_request("That change would do nothing."));
    }

    let updated = ProfilesRepo::new(context.db())
        .update(*id, &change)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(missing)?;

    record(&context, "profile.updated", &caller, &updated.name).await;

    described(&context, updated).await
}

/// `DELETE /api/v1/profiles/{id}`.
///
/// # Errors
///
/// A `404`, and a `500` when the write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    caller: Administrative,
) -> ApiResult {
    let row = row(&context, *id).await?;

    ProfilesRepo::new(context.db())
        .delete(*id)
        .await
        .map_err(|err| failed(&context, &err))?;

    record(&context, "profile.deleted", &caller, &row.name).await;

    Ok(HttpResponse::NoContent().finish())
}

/// `GET /api/v1/profiles/{id}/prefs`.
///
/// # Errors
///
/// A `404`, and a `500` when a read fails.
pub async fn get_prefs(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    _: Administrative,
) -> ApiResult {
    row(&context, *id).await?;

    let entries = ProfilesRepo::new(context.db())
        .prefs(*id)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&entries))
}

/// `PUT /api/v1/profiles/{id}/prefs` — the whole list, replacing what is there.
///
/// # Errors
///
/// A `400` for a blank key, a duplicate key, or a value its class could not
/// hold; a `404`; a `500` when the write fails.
pub async fn put_prefs(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    body: web::Json<Vec<PrefEntry>>,
    caller: Administrative,
) -> ApiResult {
    let row = row(&context, *id).await?;
    let entries = validate(body.into_inner())?;

    ProfilesRepo::new(context.db())
        .set_prefs(*id, &entries)
        .await
        .map_err(|err| failed(&context, &err))?;

    record(&context, "profile.prefs.replaced", &caller, &row.name).await;

    Ok(json_ok(&entries))
}

/// `GET /api/v1/profiles/{id}/preview` — the zip a device would receive.
///
/// # Errors
///
/// A `404`, and a `500` when the package cannot be assembled.
pub async fn preview(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    _: Administrative,
) -> ApiResult {
    let row = row(&context, *id).await?;
    let assembled = service(&context)?
        .assemble_one(&row)
        .await
        .map_err(|err| failed(&context, &err))?;

    if assembled.is_empty() {
        return Err(ApiError::bad_request(
            "That profile has no preferences and no files, so a device would receive nothing.",
        ));
    }

    let body =
        build_profile_package(&row.name, &assembled.files).map_err(|err| failed(&context, &err))?;

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, "application/zip"))
        .insert_header((
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{PROFILE_FILENAME}\""),
        ))
        .body(body))
}

/// `GET /api/v1/profiles/pref-catalog`.
///
/// # Errors
///
/// Never.
pub async fn pref_catalog(_: Administrative) -> ApiResult {
    Ok(json_ok(&catalog::catalog()))
}

/// Refuses a preference list a device could not import.
fn validate(entries: Vec<PrefEntry>) -> Result<Vec<PrefEntry>, ApiError> {
    let mut seen: Vec<&str> = Vec::with_capacity(entries.len());

    for entry in &entries {
        let key = entry.key.trim();

        if key.is_empty() {
            return Err(ApiError::bad_request("A preference needs a key."));
        }

        if seen.contains(&key) {
            return Err(ApiError::bad_request(format!(
                "The preference '{key}' is listed twice."
            )));
        }

        if !entry.class.accepts(&entry.value) {
            return Err(ApiError::bad_request(format!(
                "'{}' is not a {} that ATAK could read.",
                entry.value,
                entry.class.as_str(),
            )));
        }

        seen.push(key);
    }

    Ok(entries)
}

/// The service, or a `500` if the content store is not installed.
fn service(context: &web::Data<AppContext>) -> Result<ProfileService<'_>, ApiError> {
    let content = context.content().map_err(|err| failed(context, &err))?;

    Ok(ProfileService::new(context.db(), content))
}

/// One profile, or a `404`.
pub(super) async fn row(
    context: &web::Data<AppContext>,
    id: ProfileId,
) -> Result<ProfileRow, ApiError> {
    ProfilesRepo::new(context.db())
        .get(id)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(missing)
}

/// The profile with its counts, as the API renders it.
async fn described(context: &web::Data<AppContext>, row: ProfileRow) -> ApiResult {
    let described = service(context)?
        .describe(row)
        .await
        .map_err(|err| failed(context, &err))?;

    Ok(json_ok(&described))
}

/// What a profile that is not here is answered with.
fn missing() -> ApiError {
    ApiError::not_found("There is no profile with that identifier.")
}

/// Writes what was changed and who changed it.
pub(super) async fn record(
    context: &AppContext,
    action: &'static str,
    caller: &Administrative,
    name: &str,
) {
    let entry = AuditEntry::new(AuditCategory::Administration, action, AuditOutcome::Success)
        .subject(name)
        .actor(&caller.user.username);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a profile change in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::PrefClass;

    use super::*;

    #[test]
    fn a_preference_a_device_could_not_import_is_refused() {
        assert!(validate(vec![PrefEntry::string("", "x")]).is_err());
        assert!(
            validate(vec![
                PrefEntry::string("a", "1"),
                PrefEntry::string("a", "2"),
            ])
            .is_err(),
            "a duplicate key would render twice and import once",
        );
        assert!(validate(vec![PrefEntry::new("a", PrefClass::Boolean, "yes")]).is_err());
        assert!(validate(vec![PrefEntry::new("a", PrefClass::Integer, "7")]).is_ok());
    }
}
