//! `/api/v1/missions`: Data Sync, as an operator sees it.
//!
//! Administrative throughout, and deliberately not the Marti surface: this
//! listing hides nothing. An invite-only or password-protected mission still
//! appears, because somebody asking what is on their server is asking about all
//! of it — and the two operations here that a TAK client cannot perform are
//! taking a subscription away and deleting a mission somebody else owns.
//!
//! # Addressed by GUID only
//!
//! A mission name may be renamed, may contain a slash-free but otherwise
//! arbitrary string, and may be a bare UUID. The admin API sidesteps all of
//! that by addressing the immutable identifier, which is also what the UI has
//! after a listing.

use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use actix_web::{HttpResponse, web};
use rustak_api::{
    MissionChangeSummary, MissionDetail, MissionLayerSummary, MissionRoleUpdate,
    MissionSubscriptionSummary,
};

use crate::missions::model::Mission;
use crate::missions::{MissionService, archive};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::missions_view::{
    all_changes, change, connected_uids, failed, record, resolve, role_of, summary,
};

/// Registers every mission route, the archive before the `{guid}` that would
/// otherwise swallow nothing but is clearer read in this order.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/missions", web::get().to(list))
        .route("/missions/{guid}/archive", web::get().to(archive_download))
        .route("/missions/{guid}/changes", web::get().to(changes))
        .route(
            "/missions/{guid}/subscriptions/{uid}/role",
            web::put().to(set_role),
        )
        .route(
            "/missions/{guid}/subscriptions/{uid}",
            web::delete().to(unsubscribe),
        )
        .route("/missions/{guid}", web::get().to(get))
        .route("/missions/{guid}", web::delete().to(remove));
}

/// `GET /api/v1/missions`.
///
/// # Errors
///
/// A `500` when a read fails.
pub async fn list(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let service = MissionService::new(context.get_ref().clone());
    let rows = context
        .db()
        .missions()
        .list(crate::db::repos::MissionFilter::default())
        .await
        .map_err(|err| failed(err.into()))?;

    let mut listed = Vec::with_capacity(rows.len());

    for row in rows {
        listed.push(summary(&service, &Mission::from_row(row)).await?);
    }

    Ok(json_ok(&listed))
}

/// `GET /api/v1/missions/{guid}`.
///
/// # Errors
///
/// A `404` when no mission holds that guid, and a `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    path: web::Path<String>,
    _: Administrative,
) -> ApiResult {
    let service = MissionService::new(context.get_ref().clone());
    let mission = resolve(&service, &path.into_inner()).await?;
    let connected = connected_uids(&context);

    let subscriptions = service
        .subscriptions(&mission)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|held| MissionSubscriptionSummary {
            connected: connected.contains(&held.client_uid),
            client_uid: held.client_uid,
            username: held.username,
            role: held.role.into(),
            create_time: held.create_time,
        })
        .collect();

    let layers = service
        .layers(&mission)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|layer| MissionLayerSummary {
            uid: layer.uid,
            name: layer.name,
            kind: layer.kind,
            parent_uid: layer.parent_uid,
            position: layer.position,
            item_count: 0,
        })
        .collect();

    let detail = MissionDetail {
        change_count: u32::try_from(all_changes(&service, &mission).await?.len())
            .unwrap_or(u32::MAX),
        summary: summary(&service, &mission).await?,
        subscriptions,
        layers,
    };

    Ok(json_ok(&detail))
}

/// `GET /api/v1/missions/{guid}/changes?squashed=`.
///
/// # Errors
///
/// A `404` when no mission holds that guid, and a `500` when a read fails.
pub async fn changes(
    context: web::Data<AppContext>,
    path: web::Path<String>,
    query: web::Query<std::collections::HashMap<String, String>>,
    _: Administrative,
) -> ApiResult {
    let service = MissionService::new(context.get_ref().clone());
    let mission = resolve(&service, &path.into_inner()).await?;

    let rows = all_changes(&service, &mission).await?;
    let rows = match query.get("squashed").is_some_and(|value| value == "true") {
        true => {
            let presence = service.presence(&mission).await.map_err(failed)?;

            crate::missions::changes::squash(rows, &presence)
        }
        false => rows,
    };

    let rendered: Vec<MissionChangeSummary> = rows.iter().map(change).collect();

    Ok(json_ok(&rendered))
}

/// `PUT /api/v1/missions/{guid}/subscriptions/{clientUid}/role`.
///
/// # Errors
///
/// A `404` when the mission or the subscription is not there.
pub async fn set_role(
    context: web::Data<AppContext>,
    path: web::Path<(String, String)>,
    body: web::Json<MissionRoleUpdate>,
    caller: Administrative,
) -> ApiResult {
    let (guid, client_uid) = path.into_inner();
    let service = MissionService::new(context.get_ref().clone());
    let mission = resolve(&service, &guid).await?;
    let role = role_of(body.into_inner().role);

    let changed = service
        .set_role(&mission, Some(&client_uid), None, role)
        .await
        .map_err(failed)?;

    if changed == 0 {
        return Err(ApiError::not_found("That device is not subscribed."));
    }

    record(
        &context,
        &caller,
        "mission.subscription.role",
        &format!("{}/{client_uid}", mission.name),
    )
    .await;

    Ok(json_ok(&serde_json::json!({ "updated": changed })))
}

/// `DELETE /api/v1/missions/{guid}/subscriptions/{clientUid}`.
///
/// # Errors
///
/// A `404` when the mission or the subscription is not there.
pub async fn unsubscribe(
    context: web::Data<AppContext>,
    path: web::Path<(String, String)>,
    caller: Administrative,
) -> ApiResult {
    let (guid, client_uid) = path.into_inner();
    let service = MissionService::new(context.get_ref().clone());
    let mission = resolve(&service, &guid).await?;

    if !service
        .unsubscribe(&mission, &client_uid)
        .await
        .map_err(failed)?
    {
        return Err(ApiError::not_found("That device is not subscribed."));
    }

    record(
        &context,
        &caller,
        "mission.subscription.remove",
        &format!("{}/{client_uid}", mission.name),
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// `DELETE /api/v1/missions/{guid}?deep=`.
///
/// # Errors
///
/// A `404` when no mission holds that guid, and a `410` when it was already
/// deleted.
pub async fn remove(
    context: web::Data<AppContext>,
    path: web::Path<String>,
    query: web::Query<std::collections::HashMap<String, String>>,
    caller: Administrative,
) -> ApiResult {
    let service = MissionService::new(context.get_ref().clone());
    let mission = resolve(&service, &path.into_inner()).await?;
    let deep = query.get("deep").is_some_and(|value| value == "true");

    let deleted = service.delete(&mission, None, deep).await.map_err(failed)?;

    record(&context, &caller, "mission.delete", &deleted.name).await;

    Ok(HttpResponse::NoContent().finish())
}

/// `GET /api/v1/missions/{guid}/archive`.
///
/// # Errors
///
/// A `404` when no mission holds that guid.
pub async fn archive_download(
    context: web::Data<AppContext>,
    path: web::Path<String>,
    _: Administrative,
) -> ApiResult {
    let service = MissionService::new(context.get_ref().clone());
    let mission = resolve(&service, &path.into_inner()).await?;

    let host = service.archive_host();
    let zip = service.archive(&mission, &host).await.map_err(failed)?;
    let filename = MissionService::archive_filename(&mission);

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, "application/zip"))
        .insert_header((
            CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                archive::encode_filename(&filename)
            ),
        ))
        .body(zip))
}
