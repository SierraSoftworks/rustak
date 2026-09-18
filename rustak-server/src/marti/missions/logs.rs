//! `/missions/logs/entries` and `{n}/log` — the written record.
//!
//! Both writes answer **`201`**, which is unusual for an update and is what the
//! clients expect. The two bodies differ in exactly one way each: a `POST` must
//! **not** carry an `id` (the server assigns one) and a `PUT` must carry one
//! and must **not** carry a `servertime` (the server assigns that). Neither is
//! silently ignored, because a client that thought it had set either would be
//! writing against a record it does not have.

use actix_web::http::StatusCode;
use actix_web::web;

use crate::marti::time::TimeWindow;
use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::logs::{LogEntry, LogEntryJson};
use crate::missions::model::Mission;
use crate::missions::roles::{Permission, require};
use crate::prelude::*;
use crate::stream::ChangeKind;

use super::super::error::{MartiError, MartiResult};
use super::MissionCtx;

/// `POST /missions/logs/entries` — write a new entry.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a body carrying an `id`,
/// [`MartiError::NotFound`] for a mission it names that is not here, and
/// [`MartiError::Forbidden`] without `MISSION_WRITE` on every one of them.
#[instrument("marti.missions.log_create", skip_all)]
pub async fn create(ctx: MissionCtx, body: web::Json<LogEntryJson>) -> MartiResult {
    let body = body.into_inner();

    if body.id.is_some() {
        return Err(MartiError::InvalidRequest(
            "a new log entry must not carry an id".to_string(),
        ));
    }

    write(&ctx, &uuid::Uuid::new_v4().to_string(), body).await
}

/// `PUT /missions/logs/entries` — replace an entry.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a body with no `id` or with a
/// `servertime`, and otherwise as [`create`].
#[instrument("marti.missions.log_update", skip_all)]
pub async fn update(ctx: MissionCtx, body: web::Json<LogEntryJson>) -> MartiResult {
    let body = body.into_inner();

    let Some(id) = body.id.clone().filter(|id| !id.is_empty()) else {
        return Err(MartiError::InvalidRequest(
            "an updated log entry has to carry its id".to_string(),
        ));
    };

    if body.servertime.is_some() {
        return Err(MartiError::InvalidRequest(
            "servertime is assigned by the server".to_string(),
        ));
    }

    write(&ctx, &id, body).await
}

/// `GET /missions/logs/entries/{id}` — one entry.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such entry.
#[instrument("marti.missions.log_get", skip_all)]
pub async fn get(ctx: MissionCtx, path: web::Path<String>) -> MartiResult {
    let id = path.into_inner();

    let entry = ctx
        .service
        .log_entry(&id)
        .await?
        .ok_or_else(|| MartiError::NotFound(format!("Log entry {id}")))?;

    Ok(response::ok(kind::LOG_ENTRY, vec![entry.to_json()]))
}

/// `DELETE /missions/logs/entries/{id}` — remove one.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such entry.
#[instrument("marti.missions.log_delete", skip_all)]
pub async fn remove(ctx: MissionCtx, path: web::Path<String>) -> MartiResult {
    let id = path.into_inner();

    if !ctx.service.delete_log(&id).await? {
        return Err(MartiError::NotFound(format!("Log entry {id}")));
    }

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::LOG_ENTRY,
        None,
    ))
}

/// `GET /missions/all/logs` — every entry, for an administrator.
///
/// # Errors
///
/// [`MartiError::Forbidden`] for a caller who does not administer the server.
#[instrument("marti.missions.log_all", skip_all)]
pub async fn all(ctx: MissionCtx) -> MartiResult {
    ctx.who.require_admin()?;

    let rendered: Vec<_> = ctx
        .service
        .all_logs()
        .await?
        .iter()
        .map(LogEntry::to_json)
        .collect();

    Ok(response::ok(kind::LOG_ENTRY, rendered))
}

/// `GET {n}/log` — one mission's entries inside a window.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.log_listing", skip_all)]
pub async fn listing(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = ctx.service.resolve(&reference).await?;
    let role = ctx
        .service
        .role_for_request(&mission, &ctx.who, ctx.claims())
        .await?;

    require(role, Permission::Read)?;

    let window = TimeWindow::parse(
        query.parsed::<i64>("secago")?,
        query.get("start"),
        query.get("end"),
    )?;

    let rendered: Vec<_> = ctx
        .service
        .mission_logs(&mission, window.start, window.end)
        .await?
        .iter()
        .map(LogEntry::to_json)
        .collect();

    Ok(response::ok(kind::LOG_ENTRY, rendered))
}

/// The half both writes share: resolve, authorise, store, announce.
async fn write(ctx: &MissionCtx, id: &str, body: LogEntryJson) -> MartiResult {
    let mut missions: Vec<Mission> = Vec::new();

    for name in &body.mission_names {
        let mission = ctx
            .service
            .by_name(name)
            .await?
            .filter(|mission| !mission.is_deleted())
            .ok_or_else(|| MartiError::NotFound(format!("Mission {name}")))?;

        let role = ctx
            .service
            .role_for_request(&mission, &ctx.who, ctx.claims())
            .await?;

        require(role, Permission::Write)?;
        missions.push(mission);
    }

    let entry = ctx.service.write_log(id, &body, &missions).await?;

    for mission in &missions {
        ctx.service
            .notify_subscribers(mission, ChangeKind::Log, body.creator_uid.as_deref())
            .await?;
    }

    Ok(response::created(kind::LOG_ENTRY, entry.to_json()))
}
