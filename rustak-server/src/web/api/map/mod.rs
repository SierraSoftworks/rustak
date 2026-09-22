//! `/api/v1/map`: the situation as a map draws it, and as it changes.
//!
//! Two reads. [`features`] is everything current, parsed out of `cot_latest`;
//! [`feed`](feed::feed) is a Server-Sent Events response of what the router
//! relays from then on. They are separate so that the snapshot stays an
//! ordinary cacheable-by-nobody `GET` a script can use, and so that a page can
//! open the feed *first* and lose nothing to the gap between them.
//!
//! # Who sees what
//!
//! The rule the CoT browser uses, for the reason it uses it: a message is
//! shown to whoever was entitled to receive it — the sender's channels at send
//! time against the reader's — and an administrator sees everything. A map
//! that showed less than the reader's own device would be a worse diagnostic
//! than the list beside it; one that showed more would be a leak.
//!
//! # Reading only, for now
//!
//! Nothing here publishes. When it does, a write will name where it goes — a
//! channel or a mission — and become a CoT event injected through
//! [`Router::handle_inbound`](crate::stream::Router::handle_inbound) as though
//! a client had sent it, so that it is tagged, recorded, fanned out and fed
//! back to every open map by the path everything else already takes.

pub mod feature;
pub mod feed;
pub mod shape;

use actix_web::web;
use chrono::{Duration, Utc};
use rustak_api::MapFeature;

use crate::cot_store::latest;
use crate::prelude::*;

use super::error::{ApiResult, json_ok};
use super::extract::Authenticated;
use super::subject::failed;

/// How long after going stale a feature is still offered.
///
/// A page dims what is stale and drops it this much later, the way a TAK
/// client does, so that "it stopped reporting a minute ago" is something
/// somebody can see rather than infer from an absence.
pub const LINGER: Duration = Duration::minutes(5);

/// The most features one snapshot carries.
///
/// An ADS-B feed over a busy region is a few thousand aircraft; this is above
/// that and below the point where the response stops being one a browser
/// parses without the page noticing.
const MAX_FEATURES: u32 = 10_000;

/// Registers the map's routes.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/map/features", web::get().to(features))
        .route("/map/events", web::get().to(feed::feed));
}

/// `GET /api/v1/map/features` — everything current, newest relayed first.
///
/// # Errors
///
/// A `500` when a read fails.
pub async fn features(context: web::Data<AppContext>, caller: Authenticated) -> ApiResult {
    let rows = latest::current(context.db(), Utc::now() - LINGER, MAX_FEATURES)
        .await
        .map_err(|err| failed(&context, &err))?;

    let index = context
        .db()
        .groups()
        .index()
        .await
        .map_err(|err| failed(&context, &err))?;

    let listed: Vec<MapFeature> = rows
        .iter()
        .filter(|row| feature::drawable(&row.kind))
        .filter(|row| caller.principal.is_admin || row.visible_to(&caller.principal.groups))
        .filter_map(|row| feature::from_row(row, &index))
        .collect();

    Ok(json_ok(&listed))
}
