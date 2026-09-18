//! `{n}/contents`, `{n}/keywords` and the package import.
//!
//! Everything here needs `MISSION_WRITE`, and everything here answers with a
//! mission payload — a client that attached a file refreshes the whole Data
//! Sync from the response rather than patching its own copy.
//!
//! # The archive is M4-02's
//!
//! `GET {n}/archive` is mounted here so the route ordering is settled, and
//! answers `501` until `missions/archive.rs` lands. Replacing the body of
//! [`archive`] is the only edit that route needs.

use actix_web::web;

use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::model::KeywordTarget;
use crate::missions::render::Render;
use crate::missions::roles::{Permission, require};
use crate::missions::{Mission, MissionContentBody};
use crate::prelude::*;

use super::super::error::{MartiError, MartiResult};
use super::MissionCtx;

/// `PUT {n}/contents` — file map items and resources under a mission.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`,
/// [`MartiError::InvalidRequest`] for a body naming nothing, and
/// [`MartiError::NotFound`] for a hash Enterprise Sync does not hold.
#[instrument("marti.missions.contents.add", skip_all)]
pub async fn add(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let content: MissionContentBody = serde_json::from_slice(&body)?;

    ctx.service
        .add_content(
            &mission,
            &content,
            query.get("creatorUid"),
            chrono::Utc::now(),
        )
        .await?;

    mission_response(&ctx, &mission).await
}

/// `DELETE {n}/contents?hash=|uid=` — unfile one of them.
///
/// # Errors
///
/// As [`add`], minus the not-found: removing something that is not there is a
/// success, because the caller's intent is already satisfied.
#[instrument("marti.missions.contents.remove", skip_all)]
pub async fn remove(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;

    ctx.service
        .remove_content(
            &mission,
            query.get("hash"),
            query.get("uid"),
            query.get("creatorUid"),
        )
        .await?;

    mission_response(&ctx, &mission).await
}

/// `PUT /missions/{name}/contents/missionpackage` — import a package zip.
///
/// # Errors
///
/// [`MartiError::Duplicate`] — the `409` this route answers with — for a zip or
/// a manifest we cannot read.
#[instrument("marti.missions.contents.import", skip_all)]
pub async fn import(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let changes = ctx
        .service
        .import_package(&mission, body.to_vec(), query.get("creatorUid"))
        .await?;
    let rendered = ctx.service.render_changes(&mission, changes).await?;

    Ok(response::ok(kind::MISSION_CHANGE, rendered))
}

/// `PUT /missions/{name}/keywords` — replace a mission's keywords.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`.
#[instrument("marti.missions.keywords.set", skip_all)]
pub async fn set_keywords(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let keywords: Vec<String> = serde_json::from_slice(&body)?;
    let updated = ctx
        .service
        .set_keywords(
            &mission,
            KeywordTarget::Mission,
            keywords,
            query.get("creatorUid"),
        )
        .await?;

    mission_response(&ctx, &updated).await
}

/// `DELETE /missions/{name}/keywords` — clear them.
///
/// # Errors
///
/// As [`set_keywords`].
#[instrument("marti.missions.keywords.clear", skip_all)]
pub async fn clear_keywords(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let updated = ctx
        .service
        .set_keywords(
            &mission,
            KeywordTarget::Mission,
            Vec::new(),
            query.get("creatorUid"),
        )
        .await?;

    mission_response(&ctx, &updated).await
}

/// `DELETE /missions/{name}/keywords/{keyword}` — remove one of them.
///
/// # Errors
///
/// As [`set_keywords`].
#[instrument("marti.missions.keywords.remove", skip_all)]
pub async fn delete_keyword(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
    query: CiQuery,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let updated = ctx
        .service
        .remove_keyword(&mission, &path.1, query.get("creatorUid"))
        .await?;

    mission_response(&ctx, &updated).await
}

/// `PUT|DELETE /missions/{name}/uid/{uid}/keywords` — tag a filed map item.
///
/// # Errors
///
/// As [`set_keywords`], plus [`MartiError::NotFound`] when the item is not
/// filed under this mission.
#[instrument("marti.missions.keywords.uid", skip_all)]
pub async fn uid_keywords(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;

    ctx.service
        .set_keywords(
            &mission,
            KeywordTarget::Uid(path.1.clone()),
            keywords_of(&body)?,
            query.get("creatorUid"),
        )
        .await?;

    mission_response(&ctx, &mission).await
}

/// `PUT|DELETE /missions/{name}/content/{hash}/keywords` — tag a filed file.
///
/// # Errors
///
/// As [`uid_keywords`].
#[instrument("marti.missions.keywords.content", skip_all)]
pub async fn content_keywords(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;

    ctx.service
        .set_keywords(
            &mission,
            KeywordTarget::Hash(path.1.clone()),
            keywords_of(&body)?,
            query.get("creatorUid"),
        )
        .await?;

    mission_response(&ctx, &mission).await
}

/// `GET /missions/{name}/archive` — the mission as a Mission Package.
///
/// The filename is percent-encoded and quoted, because a mission name may carry
/// spaces and punctuation and an unquoted header would be truncated at the
/// first one.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.archive", skip_all)]
pub async fn archive(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = ctx.service.resolve(&reference).await?;
    let role = ctx
        .service
        .role_for_request(&mission, &ctx.who, ctx.claims())
        .await?;

    require(role, Permission::Read)?;

    let host = ctx.service.archive_host();
    let zip = ctx.service.archive(&mission, &host).await?;
    let filename = crate::missions::MissionService::archive_filename(&mission);

    Ok(actix_web::HttpResponse::Ok()
        .insert_header((actix_web::http::header::CONTENT_TYPE, "application/zip"))
        .insert_header((
            actix_web::http::header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                crate::missions::archive::encode_filename(&filename)
            ),
        ))
        .body(zip))
}

/// The mission this request names, once the caller may write to it.
async fn writable(ctx: &MissionCtx, reference: &MissionRef) -> Result<Mission, MartiError> {
    let mission = ctx.service.resolve(reference).await?;
    let role = ctx
        .service
        .role_for_request(&mission, &ctx.who, ctx.claims())
        .await?;

    require(role, Permission::Write)?;

    Ok(mission)
}

/// The mission payload every route in this file answers with.
async fn mission_response(ctx: &MissionCtx, mission: &Mission) -> MartiResult {
    let rendered = ctx.service.render(mission, Render::default()).await?;

    Ok(response::ok(kind::MISSION, vec![rendered]))
}

/// The keyword list a body carried, or none at all for a `DELETE`.
fn keywords_of(body: &web::Bytes) -> Result<Vec<String>, MartiError> {
    if body.is_empty() {
        return Ok(Vec::new());
    }

    Ok(serde_json::from_slice(body)?)
}
