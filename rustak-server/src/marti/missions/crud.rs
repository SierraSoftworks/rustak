//! The mission collection: list, count, read, create, update and delete.
//!
//! # `201` and `200` are the contract
//!
//! There is one route for create and update, and the status is what tells them
//! apart. A create answers `201` with a `SUBSCRIPTION` token and an
//! `ownerRole`; an update answers `200` with neither. CloudTAK persists the
//! token straight off the create response and has no other way to manage the
//! mission afterwards, so a create that answered `200` would leave it holding a
//! Data Sync it cannot edit.
//!
//! # Deleting by GUID is a query parameter
//!
//! `DELETE /Marti/api/missions?guid=…`, not `/missions/guid/{guid}`. That is
//! the one place the GUID family is spelled differently, it is what TAK Server
//! does, and it is what CloudTAK calls.

use actix_web::http::StatusCode;
use actix_web::http::header::CONTENT_TYPE;
use actix_web::{HttpRequest, web};
use uuid::Uuid;

use crate::marti::time::TimeWindow;
use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::ListFilter;
use crate::missions::dto::MissionBody;
use crate::missions::model::{Mission, MissionParams};
use crate::missions::render::Render;
use crate::missions::roles::{Permission, require};
use crate::prelude::*;

use super::super::error::{MartiError, MartiResult};
use super::{MissionCtx, allowed, resolved};

/// The shape a client that can read an emptied body says it speaks.
const STRIPPED_FROM_API_VERSION: u32 = 3;

/// `GET /Marti/api/missions` — the missions the caller may see.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a parameter that will not parse.
#[instrument("marti.missions.list", skip_all)]
pub async fn list(ctx: MissionCtx, query: CiQuery) -> MartiResult {
    let found = ctx
        .service
        .list(&ctx.who, ListFilter::from_query(&query, false)?)
        .await?;

    Ok(response::ok(kind::MISSION, render_all(&ctx, found).await?))
}

/// `GET /Marti/api/pagedmissions` — the same listing, a page at a time.
///
/// # Errors
///
/// As [`list`].
#[instrument("marti.missions.paged", skip_all)]
pub async fn paged(ctx: MissionCtx, query: CiQuery) -> MartiResult {
    let found = ctx
        .service
        .list(&ctx.who, ListFilter::from_query(&query, true)?)
        .await?;

    Ok(response::ok(kind::MISSION, render_all(&ctx, found).await?))
}

/// `GET /Marti/api/missioncount` — how many the listing would hold.
///
/// # Errors
///
/// As [`list`].
#[instrument("marti.missions.count", skip_all)]
pub async fn count(ctx: MissionCtx, query: CiQuery) -> MartiResult {
    let mut filter = ListFilter::from_query(&query, true)?;

    // Counted over the whole listing rather than one page of it, which is what
    // a client sizing its pager is asking for.
    filter.page = None;
    filter.page_size = None;

    let found = ctx.service.list(&ctx.who, filter).await?;

    Ok(response::ok(kind::MISSION, found.len()))
}

/// `GET {n}` — one mission, as a single-element array.
///
/// # Errors
///
/// [`MartiError::NotFound`], [`MartiError::Gone`] for a deleted mission, and
/// [`MartiError::Forbidden`] for a caller with no read role that speaks an
/// `API_VERSION` older than 3.
#[instrument("marti.missions.get", skip_all)]
pub async fn get(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let (mission, held) = resolved(&ctx, &reference).await?;
    let presented = query.get("password").filter(|value| !value.is_empty());

    // The one mission route that does not go straight through `allowed`: a
    // caller with no read role gets a stripped `200` rather than a `403` when
    // it speaks `API_VERSION >= 3` (`compat/missions.md` §5), so the refusal
    // has to be inspected rather than propagated.
    let (token, role) = match presented {
        Some(password) => (
            Some(ctx.service.access_token(&mission, password).await?),
            Some(crate::missions::roles::MissionRole::default_role(
                mission.default_role,
            )),
        ),
        None => (None, held),
    };

    let readable = require(role, Permission::Read);
    let stripped = readable.is_err();

    if stripped && !ctx.who.api_version.at_least(STRIPPED_FROM_API_VERSION) {
        readable?;
    }

    let rendered = ctx
        .service
        .render(
            &mission,
            Render {
                token,
                stripped,
                changes: history(&ctx, &mission, &query).await?,
                ..Render::default()
            },
        )
        .await?;

    Ok(response::ok(kind::MISSION, vec![rendered]))
}

/// `PUT|POST /Marti/api/missions/{name}` — create or update.
///
/// # Errors
///
/// [`MartiError::Validation`] for a name we will not accept,
/// [`MartiError::Unauthorized`] for an anonymous create, and
/// [`MartiError::Forbidden`] for an update the caller's role does not carry.
#[instrument("marti.missions.create", skip_all)]
pub async fn create(
    ctx: MissionCtx,
    request: HttpRequest,
    name: web::Path<String>,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let mut params = MissionParams::from_query(&query)?;

    params.device_uid = ctx
        .who
        .principal()
        .and_then(|principal| principal.device.as_ref().map(ToString::to_string));

    if !body.is_empty() {
        match is_json(&request) {
            true => params.apply_body(serde_json::from_slice::<MissionBody>(&body)?)?,
            // Anything else is a data package, imported once the mission
            // exists; see `missions/import.rs`.
            false => params.package = Some(body.to_vec()),
        }
    }

    let outcome = ctx
        .service
        .create_or_update(&ctx.who, ctx.claims(), &name, params)
        .await?;
    let created = outcome.is_created();
    let extras = match &outcome {
        crate::missions::Outcome::Created {
            token, owner_role, ..
        } => Render {
            token: Some(token.clone()),
            owner_role: Some(*owner_role),
            ..Render::default()
        },
        crate::missions::Outcome::Updated(_) => Render::default(),
    };

    let rendered = ctx.service.render(outcome.mission(), extras).await?;
    let status = match created {
        true => StatusCode::CREATED,
        false => StatusCode::OK,
    };

    Ok(response::status(
        status,
        kind::MISSION,
        Some(vec![rendered]),
    ))
}

/// `DELETE /Marti/api/missions/{name}` — retire a mission.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_DELETE`.
#[instrument("marti.missions.delete", skip_all)]
pub async fn delete(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Delete).await?;

    remove(&ctx, mission, &query).await
}

/// `DELETE /Marti/api/missions?guid=…` — the GUID spelling of the same thing.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] with TAK Server's own wording for a `guid`
/// that is missing or is not a UUID, which is what CloudTAK matches on.
#[instrument("marti.missions.delete_by_guid", skip_all)]
pub async fn delete_by_guid(ctx: MissionCtx, query: CiQuery) -> MartiResult {
    let guid = query
        .get("guid")
        .and_then(|guid| Uuid::parse_str(guid.trim_matches(['{', '}'])).ok())
        .ok_or_else(|| MartiError::InvalidRequest("Invalid mission guid in request".to_string()))?;
    let mission = allowed(&ctx, &MissionRef::Guid(guid), Permission::Delete).await?;

    remove(&ctx, mission, &query).await
}

/// The body of both delete spellings, once `MISSION_DELETE` is established.
async fn remove(ctx: &MissionCtx, mission: Mission, query: &CiQuery) -> MartiResult {
    let deleted = ctx
        .service
        .delete(
            &mission,
            query.get("creatorUid"),
            query.flag("deepDelete").get(),
        )
        .await?;
    let rendered = ctx.service.render(&deleted, Render::default()).await?;

    Ok(response::ok(kind::MISSION, vec![rendered]))
}

/// The change history a `?changes=true` read attaches.
///
/// Full history rather than the squashed delta: the `/changes` route defaults
/// the other way, and the two really are different questions.
async fn history(
    ctx: &MissionCtx,
    mission: &Mission,
    query: &CiQuery,
) -> Result<Option<Vec<crate::missions::MissionChangeJson>>, MartiError> {
    if !query.flag("changes").get() {
        return Ok(None);
    }

    let window = TimeWindow::parse(
        query.parsed::<i64>("secago")?,
        query.get("start"),
        query.get("end"),
    )?;

    Ok(Some(ctx.service.changes(mission, window, false).await?))
}

/// Renders a whole listing.
async fn render_all(
    ctx: &MissionCtx,
    found: Vec<Mission>,
) -> Result<Vec<crate::missions::MissionJson>, MartiError> {
    let mut rendered = Vec::with_capacity(found.len());

    for mission in found {
        rendered.push(ctx.service.render(&mission, Render::default()).await?);
    }

    Ok(rendered)
}

/// Whether a create's body is the JSON overrides rather than a package.
fn is_json(request: &HttpRequest) -> bool {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/json"))
}
