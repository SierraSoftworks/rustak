//! `GET /api/v1/map/features/{uid}/history`: where one thing has been.
//!
//! The history segments hold every message a uid sent, and this reads them
//! back as the features the map already draws — oldest first, because a track
//! is a line and a line has a direction. The CoT browser's
//! `/cot/{uid}/history` answers the same question as summaries, newest first,
//! for a list; this one is for a map, which wants the position, the course and
//! the speed at each fix, and wants them in the order they happened.
//!
//! # Gated by the latest row
//!
//! As the CoT browser's history is: the latest row is the only record of who
//! the sender was publishing to, so a uid this caller may not see — or one
//! nothing is held for — is a `404` before a segment is opened.

use actix_web::web;
use rustak_api::MapFeature;

use crate::cot_store::query;
use crate::prelude::*;

use super::feature;
use crate::web::api::cot::{HistoryQuery, index, names, readable_row};
use crate::web::api::error::{ApiResult, json_ok};
use crate::web::api::extract::Authenticated;
use crate::web::api::subject::failed;

/// The most fixes one track carries.
///
/// A device reporting every couple of seconds writes a fix or two thousand an
/// hour, and a track longer than that is drawn at a zoom where the fixes are
/// under one another anyway. The newest are kept: the read walks segments
/// newest first, so an old window is cheap to ask for and cheap to answer.
pub const MAX_FIXES: usize = 2_000;

/// `GET /api/v1/map/features/{uid}/history?secago&start&end&limit` — oldest
/// first.
///
/// # Errors
///
/// A `400` for a window that ends before it starts, a `404` when nothing is
/// held for that uid or the caller may not see what is, and a `500` when the
/// index or a segment cannot be read.
pub async fn history(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    request: web::Query<HistoryQuery>,
    caller: Authenticated,
) -> ApiResult {
    let row = readable_row(&context, &uid, &caller).await?;
    let (start, end) = request.window()?;
    let limit = request.limit.unwrap_or(MAX_FIXES).clamp(1, MAX_FIXES);

    // Filtered as it is read, so that a chat sent under a marker's uid does
    // not use up the fixes the answer carries.
    let events = query::history_where(
        context.db(),
        &context.config().streams_dir(),
        &uid,
        start,
        end,
        limit,
        |event| feature::drawable(&event.r#type),
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    let groups = names(&row, &index(&context).await?);

    // The read answers newest first; a track is drawn the other way.
    let mut track: Vec<MapFeature> = events
        .iter()
        .rev()
        .map(|event| {
            let received_at = event.time.to_datetime().unwrap_or(row.received_at);
            feature::from_event(event, received_at, groups.clone())
        })
        .collect();
    track.sort_by_key(|fix| fix.time);

    Ok(json_ok(&track))
}
