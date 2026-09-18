//! `/api/v1/packages`: the stored files an operator browses, uploads and
//! removes.
//!
//! The same rows `/Marti/sync/*` serves, under an API that says what it means.
//! Reading follows the Marti visibility rule — administrator, or the submitter,
//! or a channel the caller may receive from — so an operator who is not an
//! administrator sees their own installation's packages rather than all of
//! them. Writing is administrative: a package is delivered to devices, and
//! `install_on_enrollment` puts one on every device that enrols.
//!
//! # Nothing is buffered
//!
//! An upload streams through [`ingest`](crate::files::store::ingest) into the
//! content store while being hashed, and a download streams back out of it. A
//! data package is routinely tens of megabytes and the configured ceiling is
//! four hundred, so a handler that collected one would make the server's
//! resident set the number of operators uploading times the largest package.
//!
//! # A filtered page may be short
//!
//! The `missionPackage` predicate is decided in SQL and the free-text search
//! and the visibility rule are not — the first because a substring over two
//! columns and a child table is not what the index is for at these volumes, the
//! second because a caller's channels are a bit vector the row cannot be joined
//! against. So a page is narrowed after it is read, as `files::search` does.

use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use actix_web::{HttpResponse, web};
use rustak_api::{PackageSummary, PackageUpdate};

use crate::db::repos::{ResourceFilter, ResourceRow};
use crate::files::metadata::Viewer;
use crate::files::{patch, store, upload};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok, json_with};
use super::extract::{Administrative, Authenticated, Identity};
use super::packages_upload;
use super::subject::failed;

/// How many packages one page carries when the caller does not say.
const PAGE_SIZE: u32 = 100;

/// The most one page may carry however loudly the caller asks.
const MAX_PAGE_SIZE: u32 = 200;

/// Registers every package route.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/packages", web::get().to(list))
        .route("/packages", web::post().to(packages_upload::create))
        .route("/packages/{hash}", web::get().to(get))
        .route("/packages/{hash}", web::patch().to(update))
        .route("/packages/{hash}", web::delete().to(remove))
        .route("/packages/{hash}/content", web::get().to(content));
}

/// What a listing may be narrowed by.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// Only the files carrying the `missionpackage` keyword, which is what a
    /// client's data-package browser lists.
    #[serde(default, rename = "missionPackage")]
    pub mission_package: Option<bool>,

    /// Part of a name or of a keyword, matched without regard to case.
    #[serde(default)]
    pub q: Option<String>,

    #[serde(default)]
    pub page: Option<u32>,

    #[serde(default)]
    pub limit: Option<u32>,
}

/// `GET /api/v1/packages?missionPackage&q&page&limit`.
///
/// # Errors
///
/// A `500` when a read fails.
pub async fn list(
    context: web::Data<AppContext>,
    request: web::Query<ListQuery>,
    caller: Authenticated,
) -> ApiResult {
    let limit = request.limit.unwrap_or(PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
    let filter = ResourceFilter {
        keywords: match request.mission_package {
            Some(true) => vec![upload::MISSION_PACKAGE.to_string()],
            _ => Vec::new(),
        },
        limit: Some(limit),
        offset: Some(request.page.unwrap_or(0).saturating_mul(limit)),
        ..ResourceFilter::default()
    };

    let rows = context
        .db()
        .resources()
        .list(filter)
        .await
        .map_err(|err| failed(&context, &err))?;

    let viewer = viewer(&context, &caller).await?;
    let needle = request.q.as_ref().map(|q| q.trim().to_lowercase());

    let listed: Vec<PackageSummary> = rows
        .into_iter()
        .filter(|row| viewer.can_read(row))
        .filter(|row| needle.as_deref().is_none_or(|needle| matches(row, needle)))
        .map(|row| summarise(&row))
        .collect();

    Ok(json_ok(&listed))
}

/// `GET /api/v1/packages/{hash}`.
///
/// # Errors
///
/// A `404` when the hash is not stored **or** the caller may not see it, and a
/// `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    hash: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let row = readable(&context, &hash, &caller).await?;

    Ok(json_ok(&summarise(&row)))
}

/// `PATCH /api/v1/packages/{hash}`.
///
/// # Errors
///
/// A `400` for a change that would do nothing or a blank name, a `404`, and a
/// `500` when a write fails.
pub async fn update(
    context: web::Data<AppContext>,
    hash: web::Path<String>,
    body: web::Json<PackageUpdate>,
    caller: Administrative,
) -> ApiResult {
    let existing = row(&context, &hash).await?;
    let mut change = body.into_inner();

    if change.is_empty() {
        return Err(ApiError::bad_request("That change would do nothing."));
    }

    if let Some(name) = &mut change.name {
        *name = name.trim().to_string();

        if name.is_empty() {
            return Err(ApiError::bad_request("A package needs a name."));
        }
    }

    patch::apply(context.db(), &existing.hash, &change)
        .await
        .map_err(|err| failed(&context, &err))?;

    let updated = row(&context, &hash).await?;
    upload::audit(
        &**context,
        "changed",
        Some(caller.user.username.as_str()),
        &updated,
    )
    .await;

    Ok(json_ok(&summarise(&updated)))
}

/// `DELETE /api/v1/packages/{hash}` — every row with the hash, then the bytes.
///
/// The bytes go only when no surviving row, mission attachment or profile file
/// still points at them, which [`store::forget`] decides.
///
/// # Errors
///
/// A `404`, and a `500` when a write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    hash: web::Path<String>,
    caller: Administrative,
) -> ApiResult {
    let doomed = context
        .db()
        .resources()
        .list(ResourceFilter::by_hash(hash.as_str()))
        .await
        .map_err(|err| failed(&context, &err))?;

    if doomed.is_empty() {
        return Err(missing());
    }

    let ids: Vec<i64> = doomed.iter().map(|row| row.id).collect();
    context
        .db()
        .resources()
        .delete(ids)
        .await
        .map_err(|err| failed(&context, &err))?;

    for row in &doomed {
        upload::audit(
            &**context,
            "removed",
            Some(caller.user.username.as_str()),
            row,
        )
        .await;
    }

    if let Err(err) = store::forget(&**context, &hash).await {
        warn!(error = %err, "Could not remove a stored file whose last row went.");
        context.session().record_human_error(&err);
    }

    Ok(HttpResponse::NoContent().finish())
}

/// `GET /api/v1/packages/{hash}/content` — the bytes, streamed.
///
/// # Errors
///
/// A `404` as [`get`], and a `500` when the store cannot be read.
pub async fn content(
    context: web::Data<AppContext>,
    hash: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let row = readable(&context, &hash, &caller).await?;
    let store_handle = context.content().map_err(|err| failed(&context, &err))?;

    let opened = store::open_range(&store_handle, &row.hash, 0, None)
        .await
        .map_err(|_| ApiError::not_found("That file is no longer stored on this server."))?;

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, row.mime_type.clone()))
        .insert_header((
            CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                crate::marti::sync_read::urlencode(
                    row.download_path.as_deref().unwrap_or(row.name.as_str())
                )
            ),
        ))
        .insert_header((CONTENT_LENGTH, opened.length.to_string()))
        .streaming(store::chunks(opened.file, opened.length)))
}

/// Answers a freshly stored package with a `201`.
pub(super) fn created(row: &ResourceRow) -> HttpResponse {
    json_with(StatusCode::CREATED, &summarise(row))
}

/// One row as the admin API describes it.
pub(super) fn summarise(row: &ResourceRow) -> PackageSummary {
    PackageSummary {
        hash: row.hash.clone(),
        uid: row.uid.clone(),
        name: row.name.clone(),
        filename: row.filename.clone(),
        mime_type: row.mime_type.clone(),
        size: row.size,
        submitter: row.submitter.clone(),
        creator_uid: row.creator_uid.clone(),
        submission_time: row.submission_time,
        keywords: row.keywords.clone(),
        groups: row.groups.clone(),
        tool: row.tool.clone(),
        expiration: row.expiration,
        install_on_enrollment: row.install_on_enrollment,
        mission_package: row.is_mission_package,
        mission_name: row.mission_name.clone(),
    }
}

/// Whether a free-text search matches a row.
fn matches(row: &ResourceRow, needle: &str) -> bool {
    row.name.to_lowercase().contains(needle)
        || row
            .filename
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains(needle))
        || row
            .keywords
            .iter()
            .any(|keyword| keyword.to_lowercase().contains(needle))
}

/// The row for a hash, or a `404`.
pub(super) async fn row(
    context: &web::Data<AppContext>,
    hash: &str,
) -> Result<ResourceRow, ApiError> {
    context
        .db()
        .resources()
        .by_hash(hash)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(missing)
}

/// The row for a hash this caller may see, or the same `404` as a hash that is
/// not stored — telling a caller that a package exists but is out of their
/// channels is an oracle over the whole store.
async fn readable(
    context: &web::Data<AppContext>,
    hash: &str,
    caller: &Authenticated,
) -> Result<ResourceRow, ApiError> {
    let row = row(context, hash).await?;

    match viewer(context, caller).await?.can_read(&row) {
        true => Ok(row),
        false => Err(missing()),
    }
}

/// What this caller is allowed to see.
///
/// Takes the resolved identity rather than one of the two extractors, so that
/// the administrative upload and the ordinary listing ask the same question.
pub(super) async fn viewer(
    context: &web::Data<AppContext>,
    caller: &Identity,
) -> Result<Viewer, ApiError> {
    crate::files::viewer_for(
        context.db(),
        Some(caller.user.username.as_str()),
        Some(&caller.principal),
    )
    .await
    .map_err(|err| failed(context, &err))
}

/// What a hash nothing is stored under is answered with.
fn missing() -> ApiError {
    ApiError::not_found("There is no package with that hash.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, keywords: &[&str]) -> ResourceRow {
        ResourceRow {
            id: 1,
            hash: "aa".to_string(),
            uid: "uid-1".to_string(),
            name: name.to_string(),
            filename: Some("patrol-2026.zip".to_string()),
            mime_type: "application/x-zip-compressed".to_string(),
            size: 12,
            tool: "public".to_string(),
            creator_uid: None,
            submitter_id: None,
            submitter: Some("grace".to_string()),
            submission_time: chrono::Utc::now(),
            expiration: None,
            is_mission_package: true,
            groups: vec!["Blue".to_string()],
            mission_name: None,
            latitude: None,
            longitude: None,
            altitude: None,
            remarks: None,
            permissions: None,
            contacts: None,
            download_path: None,
            plugin_class_name: None,
            install_on_enrollment: false,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
        }
    }

    #[test]
    fn a_search_matches_the_name_the_filename_or_a_keyword() {
        let package = row("Patrol Brief", &["missionpackage", "Overlay"]);

        assert!(matches(&package, "patrol"), "the name, ignoring case");
        assert!(matches(&package, "2026"), "the filename it arrived under");
        assert!(matches(&package, "overlay"), "a keyword, ignoring case");
        assert!(!matches(&package, "sitrep"));
    }

    #[test]
    fn the_summary_carries_what_a_package_page_renders() {
        let summary = summarise(&row("Patrol Brief", &["missionpackage"]));

        assert_eq!(summary.hash, "aa");
        assert!(summary.mission_package);
        assert!(!summary.install_on_enrollment);
        assert_eq!(summary.groups, vec!["Blue".to_string()]);
        assert_eq!(summary.submitter.as_deref(), Some("grace"));
    }
}
