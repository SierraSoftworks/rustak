//! Copying a mission, its place in the tree, its contacts, and `send`.
//!
//! # `/contacts` and `/cot` are not enveloped
//!
//! Every other mission route answers the `{version, type, data, nodeId}`
//! envelope; these two answer a bare array and bare XML. That is TAK Server's
//! own inconsistency and both clients parse them as bare values, so it is
//! reproduced rather than tidied.

use actix_web::web;

use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::model::CopyParams;
use crate::missions::render::Render;
use crate::missions::roles::{Permission, Role};
use crate::prelude::*;

use super::super::error::{MartiError, MartiResult};
use super::{MissionCtx, allowed};

/// One connected subscriber, as `{n}/contacts` reports them.
#[derive(Debug, Clone, Serialize)]
pub struct MissionContact {
    pub callsign: String,
    #[serde(rename = "clientUid")]
    pub client_uid: String,
    /// The same value as `clientUid`; ATAK reads one and CloudTAK the other.
    pub uid: String,
    pub username: String,
    pub team: String,
    pub role: String,
    pub takv: String,
}

/// `PUT /missions/{name}/copy` — clone a mission and everything filed under it.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ` on the source, and
/// [`MartiError::Validation`] for a copy name we will not accept.
#[instrument("marti.missions.copy", skip_all)]
pub async fn copy(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Read).await?;

    let default_role =
        match query.get("defaultRole") {
            Some(requested) => Some(Role::parse(requested).ok_or_else(|| {
                MartiError::Validation(format!("{requested} is not a mission role"))
            })?),
            None => None,
        };

    let copied = ctx
        .service
        .copy(
            &ctx.who,
            &mission,
            CopyParams {
                creator_uid: query.get("creatorUid").map(str::to_string),
                copy_name: query.get("copyName").map(str::to_string),
                copy_path: query.get("copyPath").map(str::to_string),
                default_role,
                password: query.get("password").map(str::to_string),
            },
        )
        .await?;
    let rendered = ctx.service.render(&copied, Render::default()).await?;

    Ok(response::status(
        actix_web::http::StatusCode::CREATED,
        kind::MISSION,
        Some(vec![rendered]),
    ))
}

/// `GET {n}/children` — the missions filed under this one.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.children", skip_all)]
pub async fn children(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = readable(&ctx, &reference).await?;
    let found = ctx.service.children(&mission).await?;
    let mut rendered = Vec::with_capacity(found.len());

    for child in found {
        rendered.push(ctx.service.render(&child, Render::default()).await?);
    }

    Ok(response::ok(kind::MISSION, rendered))
}

/// `GET {n}/parent` — the mission this one is filed under.
///
/// A single object rather than an array, unlike almost everything else in this
/// family, and a `404` when there is no parent.
///
/// # Errors
///
/// [`MartiError::NotFound`] when the mission has no parent.
#[instrument("marti.missions.parent", skip_all)]
pub async fn parent(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = readable(&ctx, &reference).await?;
    let parent_id = mission
        .parent_id
        .ok_or_else(|| MartiError::NotFound(format!("a parent for {}", mission.name)))?;
    let parent = ctx
        .service
        .by_id(parent_id)
        .await?
        .ok_or_else(|| MartiError::NotFound(format!("a parent for {}", mission.name)))?;
    let rendered = ctx.service.render(&parent, Render::default()).await?;

    Ok(response::ok(kind::MISSION, rendered))
}

/// `PUT {n}/parent/{parent}` — file a mission under another.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE` on the child.
#[instrument("marti.missions.set_parent", skip_all)]
pub async fn set_parent(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String)>,
) -> MartiResult {
    let child = writable(&ctx, &reference).await?;
    let parent = ctx.service.resolve(&MissionRef::parse(&path.1)?).await?;

    ctx.service.set_parent(&child, Some(&parent)).await?;

    Ok(response::status::<()>(
        actix_web::http::StatusCode::OK,
        kind::MISSION,
        None,
    ))
}

/// `DELETE {n}/parent` — unfile it.
///
/// # Errors
///
/// As [`set_parent`].
#[instrument("marti.missions.clear_parent", skip_all)]
pub async fn clear_parent(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let child = writable(&ctx, &reference).await?;

    ctx.service.set_parent(&child, None).await?;

    Ok(response::status::<()>(
        actix_web::http::StatusCode::OK,
        kind::MISSION,
        None,
    ))
}

/// `POST {n}/send?contacts=…` — invite a list of devices.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for an empty `contacts`.
#[instrument("marti.missions.send", skip_all)]
pub async fn send(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = readable(&ctx, &reference).await?;

    if query.strings("contacts").is_empty() {
        return Err(MartiError::InvalidRequest(
            "contacts is required".to_string(),
        ));
    }

    // Every contact becomes a `clientUid` invitation, which is what makes the
    // mission reachable by a device that could not otherwise see it.
    for contact in query.strings("contacts") {
        let invitation = ctx
            .service
            .invite(
                &mission,
                "clientUid",
                &contact,
                query.get("creatorUid"),
                crate::missions::Role::Subscriber,
            )
            .await?;

        if let Some(token) = invitation.token.clone() {
            ctx.service.notify_invited(
                &mission,
                query.get("creatorUid"),
                &token,
                invitation.role,
                vec![contact],
            );
        }
    }

    let rendered = ctx.service.render(&mission, Render::default()).await?;

    Ok(response::ok(kind::MISSION, vec![rendered]))
}

/// `GET {n}/contacts` — the subscribers that are connected right now.
///
/// A bare array, not an envelope.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.contacts", skip_all)]
pub async fn contacts(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = readable(&ctx, &reference).await?;
    let subscribed = ctx.service.subscriptions(&mission).await?;
    let connected = match ctx.service.context().has_live() {
        true => ctx.service.context().live()?.snapshot(),
        false => Vec::new(),
    };

    let found: Vec<MissionContact> = connected
        .into_iter()
        .filter(|endpoint| {
            subscribed
                .iter()
                .any(|subscription| subscription.client_uid == endpoint.uid)
        })
        .map(|endpoint| MissionContact {
            callsign: endpoint.callsign,
            client_uid: endpoint.uid.clone(),
            uid: endpoint.uid,
            username: endpoint.username,
            team: endpoint.team,
            role: endpoint.role,
            takv: endpoint.takv,
        })
        .collect();

    Ok(response::bare_json(&found))
}

/// The mission this request names, once the caller may read it.
async fn readable(
    ctx: &MissionCtx,
    reference: &MissionRef,
) -> Result<crate::missions::Mission, MartiError> {
    allowed(ctx, reference, Permission::Read).await
}

/// The same, once the caller may write to it.
async fn writable(
    ctx: &MissionCtx,
    reference: &MissionRef,
) -> Result<crate::missions::Mission, MartiError> {
    allowed(ctx, reference, Permission::Write).await
}
