//! `{n}/maplayers`, `{n}/externaldata` and `{n}/feed` — the opaque three.
//!
//! Split from [`layers`](super::layers) because they are a different thing:
//! the layer tree is structure this server maintains, and these three are
//! records it stores and hands back untouched. A map layer's body in
//! particular is never parsed — a client that adds a field of its own reads it
//! back unchanged.

use actix_web::http::StatusCode;
use actix_web::web;

use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::external::ExternalData;
use crate::prelude::*;
use crate::stream::ChangeKind;

use super::super::error::{MartiError, MartiResult};
use super::MissionCtx;
use super::layers::writable;

/// `POST|PUT {n}/maplayers` — store a map layer verbatim.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`.
#[instrument("marti.missions.maplayer_put", skip_all)]
pub async fn put_map_layer(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: web::Json<serde_json::Value>,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let creator = query.get("creatorUid").map(ToOwned::to_owned);

    let stored = ctx
        .service
        .put_map_layer(&mission, body.into_inner(), creator.as_deref())
        .await?;

    ctx.service
        .notify_subscribers(&mission, ChangeKind::Content, creator.as_deref())
        .await?;

    Ok(response::ok(kind::MAP_LAYER, stored.body))
}

/// `DELETE {n}/maplayers/{uid}` — remove one.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such map layer.
#[instrument("marti.missions.maplayer_delete", skip_all)]
pub async fn delete_map_layer(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
) -> MartiResult {
    let (_, uid) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;

    if !ctx.service.delete_map_layer(&mission, &uid).await? {
        return Err(MartiError::NotFound(format!("Map layer {uid}")));
    }

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MAP_LAYER,
        None,
    ))
}

/// `POST {n}/externaldata` — attach an external tool's data.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a record with no name.
#[instrument("marti.missions.externaldata", skip_all)]
pub async fn put_external(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: web::Json<ExternalData>,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let creator = query.get("creatorUid").map(ToOwned::to_owned);

    let stored = ctx
        .service
        .put_external_data(&mission, body.into_inner(), creator.as_deref())
        .await?;

    ctx.service
        .notify_subscribers(&mission, ChangeKind::ExternalData, creator.as_deref())
        .await?;

    Ok(response::created(kind::EXTERNAL_DATA, stored))
}

/// `DELETE {n}/externaldata/{id}` — remove one.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such record.
#[instrument("marti.missions.externaldata_delete", skip_all)]
pub async fn delete_external(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
) -> MartiResult {
    let (_, id) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;

    if !ctx.service.delete_external_data(&mission, &id).await? {
        return Err(MartiError::NotFound(format!("External data {id}")));
    }

    ctx.service
        .notify_subscribers(&mission, ChangeKind::ExternalData, None)
        .await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::EXTERNAL_DATA,
        None,
    ))
}

/// `POST {n}/feed` — record a data feed, which nothing consumes.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`.
#[instrument("marti.missions.feed_put", skip_all)]
pub async fn put_feed(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: Option<web::Json<serde_json::Value>>,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let body = body.map_or_else(|| serde_json::json!({}), web::Json::into_inner);
    let uid = body
        .get("uid")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    ctx.service
        .put_feed(&mission, &uid, body, query.get("creatorUid"))
        .await?;

    Ok(response::status::<()>(StatusCode::OK, kind::MISSION, None))
}

/// `DELETE {n}/feed/{uid}` — forget one.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`.
#[instrument("marti.missions.feed_delete", skip_all)]
pub async fn delete_feed(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
) -> MartiResult {
    let (_, uid) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;

    ctx.service.delete_feed(&mission, &uid).await?;

    Ok(response::status::<()>(StatusCode::OK, kind::MISSION, None))
}
