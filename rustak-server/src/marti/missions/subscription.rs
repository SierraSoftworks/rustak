//! Subscribing, roles, tokens, passwords and expiry.
//!
//! # The two `type` strings
//!
//! The singular `/subscription` routes answer the fully-qualified Java class
//! name and the plural `/subscriptions` ones answer the bare literal. That is
//! not a mistake in the transcription: TAK Server really does emit two
//! different strings for the same model, and a client that switches on `type`
//! rejects the other one.
//!
//! # A subscribe answers `201`
//!
//! Not `200`, and always with a token. CloudTAK stores that token and replays
//! it on every later call, and it re-subscribes every Data Sync on every stream
//! reconnect — so this route staying cheap and reliable is what keeps a
//! reconnect from silently dropping a layer.

use actix_web::http::StatusCode;
use actix_web::web;

use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::render::Render;
use crate::missions::roles::{Permission, Role};
use crate::missions::{SubscribeReq, role_json, subscription_json};
use crate::prelude::*;

use super::super::error::{MartiError, MartiResult};
use super::{MissionCtx, allowed, resolved};

/// The `API_VERSION` a client must claim for the nested mission.
const NESTED_FROM_API_VERSION: u32 = 3;

/// `PUT {n}/subscription` — subscribe a device and mint its token.
///
/// One of the three mission routes that deliberately does **not** go through
/// `allowed`: it is how a caller *acquires* a role, so the credential it checks
/// is the password, the standing invitation or the token in the request
/// (`compat/missions.md` §9), not a role it already holds.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] with neither `uid` nor `topic`, and
/// [`MartiError::Forbidden`] for a password or an invitation that does not
/// hold.
#[instrument("marti.missions.subscribe", skip_all)]
pub async fn subscribe(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = ctx.service.resolve(&reference).await?;
    let client_uid = client_uid(&query)?;
    let token_role = ctx
        .service
        .role_from_token(
            &mission,
            &[
                crate::auth::TokenType::Invitation,
                crate::auth::TokenType::Subscription,
                crate::auth::TokenType::Access,
            ],
            &ctx.who,
            ctx.claims(),
        )
        .await?;

    let subscription = ctx
        .service
        .subscribe(
            &mission,
            SubscribeReq {
                client_uid,
                username: ctx.who.username().map(str::to_string),
                password: query.get("password").map(str::to_string),
                token_role,
                invited_role: None,
            },
        )
        .await?;

    let nested = match ctx.who.api_version.at_least(NESTED_FROM_API_VERSION) {
        true => Some(ctx.service.render(&mission, Render::default()).await?),
        false => None,
    };

    Ok(response::status(
        StatusCode::CREATED,
        kind::MISSION_SUBSCRIPTION_FQCN,
        Some(subscription_json(&subscription, nested, true)),
    ))
}

/// `GET {n}/subscription?uid=` — one device's subscription.
///
/// **No token is minted here and none is rendered.** R-02 C1: this route used
/// to call the service's minting read with no permission check at all, so an
/// anonymous caller naming a mission and a predictable subscriber uid was handed
/// a `MISSION_OWNER` credential. The read is now the stored row
/// ([`MissionService::stored_subscription`]) behind `MISSION_READ`, which is
/// what `compat/missions.md` §9 describes — the token is minted by `PUT`
/// (§10), and `/subscriptions/roles` is the deliberately token-free listing.
///
/// [`MissionService::stored_subscription`]: crate::missions::MissionService::stored_subscription
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`, and
/// [`MartiError::NotFound`] when that device is not subscribed.
#[instrument("marti.missions.subscription", skip_all)]
pub async fn get(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Read).await?;
    let found = ctx
        .service
        .stored_subscription(&mission, &client_uid(&query)?)
        .await?
        .ok_or_else(|| MartiError::NotFound("that subscription".to_string()))?;

    Ok(response::ok(
        kind::MISSION_SUBSCRIPTION_FQCN,
        subscription_json(&found, None, false),
    ))
}

/// `DELETE {n}/subscription?uid=` — unsubscribe a device.
///
/// `disconnectOnly` is accepted and ignored; see
/// [`MissionService::unsubscribe`](crate::missions::MissionService::unsubscribe).
///
/// Behind `MISSION_READ` rather than `MISSION_WRITE`: unsubscribing is what a
/// read-only subscriber does when it leaves, and requiring write would strand
/// them. R-02 H4 — it used to be behind nothing at all, so anybody could
/// unsubscribe anybody from anything.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`, and
/// [`MartiError::InvalidRequest`] with neither `uid` nor `topic`.
#[instrument("marti.missions.unsubscribe", skip_all)]
pub async fn unsubscribe(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Read).await?;

    ctx.service
        .unsubscribe(&mission, &client_uid(&query)?)
        .await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_SUBSCRIPTION_FQCN,
        None,
    ))
}

/// `POST {n}/subscription` — set several subscribers' roles at once.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_SET_ROLE`.
#[instrument("marti.missions.set_roles", skip_all)]
pub async fn set_roles(ctx: MissionCtx, reference: MissionRef, body: web::Bytes) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::SetRole).await?;
    let requested: Vec<crate::missions::MissionSubscriptionJson> = serde_json::from_slice(&body)?;

    for entry in requested {
        if let Some(role) = Role::parse(&entry.role.kind) {
            ctx.service
                .set_role(&mission, Some(&entry.client_uid), None, role)
                .await?;
        }
    }

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_SUBSCRIPTION,
        None,
    ))
}

/// `GET {n}/subscriptions` — the client uids that are subscribed.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.subscriptions", skip_all)]
pub async fn list(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Read).await?;
    let uids: Vec<String> = ctx
        .service
        .subscriptions(&mission)
        .await?
        .into_iter()
        .map(|subscription| subscription.client_uid)
        .collect();

    Ok(response::ok(kind::MISSION_SUBSCRIPTION, uids))
}

/// `GET {n}/subscriptions/roles` — the same, with roles and without tokens.
///
/// # Errors
///
/// As [`list`].
#[instrument("marti.missions.subscription_roles", skip_all)]
pub async fn roles(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Read).await?;
    let found: Vec<_> = ctx
        .service
        .subscriptions(&mission)
        .await?
        .iter()
        .map(|subscription| subscription_json(subscription, None, false))
        .collect();

    Ok(response::ok(kind::MISSION_SUBSCRIPTION, found))
}

/// `GET /missions/all/subscriptions` — every subscription, by mission name.
///
/// # Errors
///
/// [`MartiError::Forbidden`] for a caller who is not an administrator.
#[instrument("marti.missions.all_subscriptions", skip_all)]
pub async fn all(ctx: MissionCtx) -> MartiResult {
    ctx.who.require_admin()?;

    Ok(response::ok(
        kind::MISSION_SUBSCRIPTION,
        ctx.service.all_subscriptions(false).await?,
    ))
}

/// `GET /missions/all/subscriptions/guid` — the same, keyed by guid.
///
/// # Errors
///
/// As [`all`].
#[instrument("marti.missions.all_subscriptions_guid", skip_all)]
pub async fn all_by_guid(ctx: MissionCtx) -> MartiResult {
    ctx.who.require_admin()?;

    Ok(response::ok(
        kind::MISSION_SUBSCRIPTION,
        ctx.service.all_subscriptions(true).await?,
    ))
}

/// `GET {n}/role` — the role this request carries.
///
/// Deliberately not behind `allowed`: "what may I do here" has to be answerable
/// by somebody who may do nothing, which is what the absent `data` says.
///
/// The envelope carries no `data` at all when the caller has no role, which is
/// how a client tells "read only" from "nothing".
///
/// # Errors
///
/// [`MartiError::NotFound`] or [`MartiError::Gone`] for the mission itself.
#[instrument("marti.missions.role", skip_all)]
pub async fn role(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let (_, held) = resolved(&ctx, &reference).await?;

    Ok(response::status(
        StatusCode::OK,
        kind::MISSION_ROLE,
        held.map(|role| role_json(role.kind)),
    ))
}

/// `PUT {n}/role?clientUid=|username=&role=` — change somebody's role.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_SET_ROLE`, and
/// [`MartiError::Validation`] for a role we do not know.
#[instrument("marti.missions.set_role", skip_all)]
pub async fn set_role(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::SetRole).await?;
    let requested = query
        .get("role")
        .ok_or_else(|| MartiError::InvalidRequest("role is required".to_string()))?;
    let role = Role::parse(requested)
        .ok_or_else(|| MartiError::Validation(format!("{requested} is not a mission role")))?;

    ctx.service
        .set_role(
            &mission,
            query.get("clientUid"),
            query.get("username"),
            role,
        )
        .await?;

    Ok(response::status::<()>(
        StatusCode::OK,
        kind::MISSION_ROLE,
        None,
    ))
}

/// `GET /missions/{name}/token?password=` — mint an `ACCESS` token.
///
/// Deliberately not behind `allowed`: the password **is** the credential, and
/// a wrong one is the `403` (`compat/missions.md` §9).
///
/// `201`, not `200`: TAK Server answers a created token with a created status
/// and CloudTAK follows it.
///
/// # Errors
///
/// [`MartiError::Forbidden`] when the password does not match.
#[instrument("marti.missions.token", skip_all)]
pub async fn access_token(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = ctx.service.resolve(&reference).await?;
    let password = query
        .get("password")
        .ok_or_else(|| MartiError::InvalidRequest("password is required".to_string()))?;
    let token = ctx.service.access_token(&mission, password).await?;

    Ok(response::status(
        StatusCode::CREATED,
        kind::STRING,
        Some(token),
    ))
}

/// `PUT {n}/password?password=` — set a mission's password.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_SET_PASSWORD`.
#[instrument("marti.missions.set_password", skip_all)]
pub async fn set_password(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::SetPassword).await?;

    ctx.service
        .set_password(&mission, query.get("password"))
        .await?;

    Ok(response::status::<()>(StatusCode::OK, kind::MISSION, None))
}

/// `DELETE {n}/password` — clear it.
///
/// # Errors
///
/// As [`set_password`].
#[instrument("marti.missions.clear_password", skip_all)]
pub async fn clear_password(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::SetPassword).await?;

    ctx.service.set_password(&mission, None).await?;

    Ok(response::status::<()>(StatusCode::OK, kind::MISSION, None))
}

/// `PUT {n}/expiration?expiration=` — set or clear a mission's expiry.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for an `expiration` that will not parse, and
/// [`MartiError::Forbidden`] for a caller who is not the owner.
#[instrument("marti.missions.set_expiration", skip_all)]
pub async fn set_expiration(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::UpdateGroups).await?;
    let expiration = query
        .parsed::<i64>("expiration")?
        .ok_or_else(|| MartiError::InvalidRequest("expiration is required".to_string()))?;

    ctx.service
        .set_expiration(&mission, Some(expiration))
        .await?;

    Ok(response::status::<()>(StatusCode::OK, kind::MISSION, None))
}

/// The device a subscription request named, in either spelling.
fn client_uid(query: &CiQuery) -> Result<String, MartiError> {
    query
        .get("uid")
        .or_else(|| query.get("topic"))
        .filter(|uid| !uid.is_empty())
        .map(str::to_string)
        .ok_or_else(|| MartiError::InvalidRequest("uid or topic is required".to_string()))
}
