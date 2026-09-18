//! `{n}/layers`, `{n}/maplayers`, `{n}/externaldata` and `{n}/feed`.
//!
//! The layer tree is the folder structure ATAK's Data Sync view draws and
//! CloudTAK's ETL writes into; the other three are opaque records a mission
//! carries and this server never interprets.
//!
//! # A layer delete unfiles, it does not remove
//!
//! `DELETE {n}/layers?uid=` takes the layer and its descendants and leaves
//! everything that was filed under them in the mission, unfiled. A client that
//! deletes a folder expects its markers to move to the root.

use actix_web::http::StatusCode;
use actix_web::web;

use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::layers::{MissionLayer, MissionLayerJson, NewLayer};
use crate::missions::model::Mission;
use crate::missions::roles::{Permission, require};
use crate::prelude::*;
use crate::stream::ChangeKind;

use super::super::error::{MartiError, MartiResult};
use super::MissionCtx;

/// `GET {n}/layers` — the tree, with what is filed under each node.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.layers", skip_all)]
pub async fn listing(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = readable(&ctx, &reference).await?;
    let tree = build_tree(&ctx, &mission).await?;

    Ok(response::ok(kind::MISSION_LAYER, tree))
}

/// `PUT {n}/layers` — create one.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`, and
/// [`MartiError::InvalidRequest`] for a type TAK does not define.
#[instrument("marti.missions.layer_create", skip_all)]
pub async fn create(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let creator = query.get("creatorUid").map(ToOwned::to_owned);

    let layer = ctx
        .service
        .add_layer(
            &mission,
            NewLayer {
                uid: query.get("uid").map(ToOwned::to_owned),
                name: query.get("name").map(ToOwned::to_owned),
                kind: query.get("type").unwrap_or("GROUP").to_string(),
                parent_uid: query.get("parentUid").map(ToOwned::to_owned),
                after_uid: query.get("afterUid").map(ToOwned::to_owned),
                creator_uid: creator.clone(),
                data: None,
            },
        )
        .await?;

    ctx.service
        .notify_layer(&mission, layer.to_notice(), creator.as_deref())
        .await?;

    Ok(response::ok(kind::MISSION_LAYER, render(&layer)))
}

/// `PUT {n}/layers/{uid}/name` — rename one.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such layer.
#[instrument("marti.missions.layer_rename", skip_all)]
pub async fn rename(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
    query: CiQuery,
) -> MartiResult {
    let (_, uid) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;
    let name = query.get("name").unwrap_or_default();

    if !ctx.service.rename_layer(&mission, &uid, name).await? {
        return Err(MartiError::NotFound(format!("Mission layer {uid}")));
    }

    announce(&ctx, &mission, &uid, query.get("creatorUid")).await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_LAYER,
        None,
    ))
}

/// `PUT {n}/layers/{uid}/position` — move one among its siblings.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such layer.
#[instrument("marti.missions.layer_position", skip_all)]
pub async fn position(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
    query: CiQuery,
) -> MartiResult {
    let (_, uid) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;

    if !ctx
        .service
        .move_layer(&mission, &uid, None, query.get("afterUid"))
        .await?
    {
        return Err(MartiError::NotFound(format!("Mission layer {uid}")));
    }

    announce(&ctx, &mission, &uid, query.get("creatorUid")).await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_LAYER,
        None,
    ))
}

/// `PUT {n}/layers/parent` — reparent one.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no such layer.
#[instrument("marti.missions.layer_parent", skip_all)]
pub async fn reparent(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let uid = query
        .get("layerUid")
        .ok_or_else(|| MartiError::InvalidRequest("layerUid is required".to_string()))?
        .to_string();

    if !ctx
        .service
        .move_layer(
            &mission,
            &uid,
            query.get("parentUid"),
            query.get("afterUid"),
        )
        .await?
    {
        return Err(MartiError::NotFound(format!("Mission layer {uid}")));
    }

    announce(&ctx, &mission, &uid, query.get("creatorUid")).await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_LAYER,
        None,
    ))
}

/// `DELETE {n}/layers?uid=…` — remove layers, unfiling what was in them.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`.
#[instrument("marti.missions.layer_delete", skip_all)]
pub async fn remove(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let uids = query.all("uid");

    ctx.service.delete_layers(&mission, uids).await?;
    ctx.service
        .notify_subscribers(&mission, ChangeKind::Layer, query.get("creatorUid"))
        .await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_LAYER,
        None,
    ))
}

/// The rendered tree: roots first, each with its children and its items.
async fn build_tree(
    ctx: &MissionCtx,
    mission: &Mission,
) -> Result<Vec<MissionLayerJson>, MartiError> {
    let layers = ctx.service.layers(mission).await?;
    let uids = ctx.db_uids(mission).await?;
    let maps = ctx.service.map_layers(mission).await?;

    let roots: Vec<&MissionLayer> = layers
        .iter()
        .filter(|layer| layer.parent_uid.is_none())
        .collect();

    Ok(roots
        .into_iter()
        .map(|root| node(root, &layers, &uids, &maps))
        .collect())
}

/// One node with everything hanging off it.
fn node(
    layer: &MissionLayer,
    all: &[MissionLayer],
    uids: &[(String, Option<String>)],
    maps: &[crate::missions::external::MapLayer],
) -> MissionLayerJson {
    MissionLayerJson {
        uid: layer.uid.clone(),
        name: layer.name.clone(),
        kind: layer.kind.clone(),
        parent_uid: layer.parent_uid.clone(),
        mission_layers: all
            .iter()
            .filter(|child| child.parent_uid.as_deref() == Some(layer.uid.as_str()))
            .map(|child| node(child, all, uids, maps))
            .collect(),
        uids: uids
            .iter()
            .filter(|(_, filed)| filed.as_deref() == Some(layer.uid.as_str()))
            .map(|(uid, _)| serde_json::json!({ "data": uid }))
            .collect(),
        contents: Vec::new(),
        // Map layers hang off a `MAPLAYER` node, which is the only place a
        // client looks for them.
        maplayers: match layer.kind == crate::missions::layers::MAP_LAYER {
            true => maps.iter().map(|map| map.body.clone()).collect(),
            false => Vec::new(),
        },
    }
}

impl MissionCtx {
    /// The uids filed under a mission, with the layer each sits in.
    async fn db_uids(
        &self,
        mission: &Mission,
    ) -> Result<Vec<(String, Option<String>)>, MartiError> {
        Ok(self
            .service
            .layer_items(mission)
            .await?
            .into_iter()
            .map(|item| (item.uid, item.layer_uid))
            .collect())
    }
}

/// The wire shape of one layer on its own.
fn render(layer: &MissionLayer) -> MissionLayerJson {
    MissionLayerJson {
        uid: layer.uid.clone(),
        name: layer.name.clone(),
        kind: layer.kind.clone(),
        parent_uid: layer.parent_uid.clone(),
        ..MissionLayerJson::default()
    }
}

/// Sends the `t-x-m-c-h` a layer edit is worth.
async fn announce(
    ctx: &MissionCtx,
    mission: &Mission,
    uid: &str,
    creator_uid: Option<&str>,
) -> Result<(), MartiError> {
    let Some(layer) = ctx.service.layer(mission, uid).await? else {
        return Ok(());
    };

    ctx.service
        .notify_layer(mission, layer.to_notice(), creator_uid)
        .await?;

    Ok(())
}

/// The mission, once the caller is known to be allowed to read it.
async fn readable(ctx: &MissionCtx, reference: &MissionRef) -> Result<Mission, MartiError> {
    let mission = ctx.service.resolve(reference).await?;
    let role = ctx
        .service
        .role_for_request(&mission, &ctx.who, ctx.claims())
        .await?;

    require(role, Permission::Read)?;

    Ok(mission)
}

/// The mission, once the caller is known to be allowed to write to it.
pub(super) async fn writable(
    ctx: &MissionCtx,
    reference: &MissionRef,
) -> Result<Mission, MartiError> {
    let mission = ctx.service.resolve(reference).await?;
    let role = ctx
        .service
        .role_for_request(&mission, &ctx.who, ctx.claims())
        .await?;

    require(role, Permission::Write)?;

    Ok(mission)
}
