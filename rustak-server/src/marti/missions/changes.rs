//! `{n}/changes` and `{n}/cot` — what happened, and what is there now.
//!
//! The two defaults are genuinely different and both are relied on: `/changes`
//! squashes unless told not to, while the `?changes=true` form of a mission
//! read returns the full history. See [`crate::missions::changes`] for the
//! squash itself.

use crate::marti::time::TimeWindow;
use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::roles::{Permission, require};
use crate::prelude::*;

use super::super::error::MartiResult;
use super::MissionCtx;

/// `GET {n}/changes` — the change log inside a window.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`, and
/// [`MartiError::InvalidRequest`] for a window that will not parse.
///
/// [`MartiError::Forbidden`]: crate::marti::MartiError::Forbidden
/// [`MartiError::InvalidRequest`]: crate::marti::MartiError::InvalidRequest
#[instrument("marti.missions.changes", skip_all)]
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
    // `squashed` defaults to true here and to false on a mission read; the two
    // are different questions about the same rows.
    let squashed = query.get("squashed").is_none_or(|value| value == "true");
    let changes = ctx.service.changes(&mission, window, squashed).await?;

    Ok(response::ok(kind::MISSION_CHANGE, changes))
}

/// `GET {n}/cot` — the latest event for every item filed under a mission.
///
/// Answers `<events></events>` for an empty mission rather than a `404`:
/// CloudTAK renders this document directly, and a missing one is a broken
/// layer where an empty one is simply a layer with nothing on it.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
///
/// [`MartiError::Forbidden`]: crate::marti::MartiError::Forbidden
#[instrument("marti.missions.cot", skip_all)]
pub async fn cot(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = ctx.service.resolve(&reference).await?;
    let role = ctx
        .service
        .role_for_request(&mission, &ctx.who, ctx.claims())
        .await?;

    require(role, Permission::Read)?;

    let document = ctx
        .service
        .cot_events_xml(&mission, query.get("path"))
        .await?;

    Ok(response::xml(document))
}
