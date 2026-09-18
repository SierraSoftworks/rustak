//! Rendering a stored mission as the admin API reports it.
//!
//! Split from [`missions`](super::missions) so that the route file stays what a
//! route file should be — parameters in, service call, response out — and the
//! mapping between the service's types and `rustak-api`'s lives in one place
//! somebody can read against the DTO definitions.

use rustak_api::{
    AuditCategory, AuditOutcome, MissionChangeKind, MissionChangeSummary, MissionGuid,
    MissionRoleKind, MissionSummary, UidDetails,
};

use crate::db::AuditEntry;
use crate::marti::MissionRef;
use crate::missions::MissionService;
use crate::missions::model::Mission;
use crate::missions::roles::Role;
use crate::prelude::*;

use super::error::ApiError;
use super::extract::Administrative;

/// The live mission a guid names.
///
/// A deleted one is a `410` rather than a `404`: the row is deliberately kept
/// so that a client syncing late is told the mission went, and the same
/// distinction is worth making to the operator who just clicked Delete twice.
pub(super) async fn resolve(service: &MissionService, guid: &str) -> Result<Mission, ApiError> {
    match service
        .resolve(&MissionRef::Guid(*parse(guid)?.as_uuid()))
        .await
    {
        Ok(mission) => Ok(mission),
        Err(crate::marti::MartiError::NotFound(_)) => {
            Err(ApiError::not_found("There is no mission with that id."))
        }
        Err(crate::marti::MartiError::Gone(_)) => Err(deleted()),
        Err(err) => Err(failed(err)),
    }
}

/// The mission a guid names, whether or not it has been deleted.
///
/// Read straight from the repository rather than through
/// [`MissionService::resolve`], which turns a deleted row into an error — the
/// detail page is the one caller that wants the row *because* it is deleted, so
/// that it can say when and what was in it instead of an empty page.
pub(super) async fn resolve_any(context: &AppContext, guid: &str) -> Result<Mission, ApiError> {
    context
        .db()
        .missions()
        .by_guid(*parse(guid)?.as_uuid())
        .await
        .map_err(|err| failed(err.into()))?
        .map(Mission::from_row)
        .ok_or_else(|| ApiError::not_found("There is no mission with that id."))
}

/// What a caller asking after a deleted mission is told.
pub(super) fn deleted() -> ApiError {
    ApiError::gone("That mission has been deleted.")
}

/// A guid, or the `400` that says it was not one.
fn parse(guid: &str) -> Result<MissionGuid, ApiError> {
    MissionGuid::parse(guid).map_err(|_| ApiError::bad_request("That is not a mission identifier."))
}

/// How many items and resources are filed under each layer of a mission.
///
/// Counted over the rows the mission's own contents are read from rather than
/// asked for as a second aggregate: both tables are read by primary key over
/// one mission and both carry `layer_uid`, so the tally costs nothing beyond
/// the reads the detail already needs. A layer with nothing under it is absent
/// from the map and reads as zero.
pub(super) async fn layer_counts(
    context: &AppContext,
    mission: &Mission,
) -> Result<std::collections::HashMap<String, u32>, ApiError> {
    let repo = context.db().mission_contents();
    let uids = repo
        .uids(mission.id)
        .await
        .map_err(|err| failed(err.into()))?;
    let contents = repo
        .contents(mission.id)
        .await
        .map_err(|err| failed(err.into()))?;

    let filed = uids
        .into_iter()
        .filter_map(|row| row.layer_uid)
        .chain(contents.into_iter().filter_map(|row| row.layer_uid));

    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for layer_uid in filed {
        *counts.entry(layer_uid).or_default() += 1;
    }

    Ok(counts)
}

/// The listing form of one mission, with its counts.
pub(super) async fn summary(
    service: &MissionService,
    mission: &Mission,
) -> Result<MissionSummary, ApiError> {
    let subscriptions = service.subscriptions(mission).await.map_err(failed)?;
    let uids = service.layer_items(mission).await.map_err(failed)?;
    let contents = service
        .filed_contents_count(mission)
        .await
        .map_err(failed)?;

    Ok(MissionSummary {
        guid: MissionGuid::from_uuid(mission.guid),
        name: mission.name.clone(),
        description: Some(mission.description.clone()).filter(|text| !text.is_empty()),
        tool: mission.tool.clone(),
        creator_uid: mission.creator_uid.clone(),
        create_time: mission.create_time,
        groups: mission
            .effective_groups()
            .iter()
            .filter_map(|name| GroupName::parse(name).ok())
            .collect(),
        keywords: mission.keywords.clone(),
        subscriber_count: u32::try_from(subscriptions.len()).unwrap_or(u32::MAX),
        uid_count: u32::try_from(uids.len()).unwrap_or(u32::MAX),
        content_count: u32::try_from(contents).unwrap_or(u32::MAX),
        password_protected: mission.is_password_protected(),
        invite_only: mission.invite_only,
        default_role: mission.default_role.into(),
        expiration: mission
            .expiration
            .filter(|seconds| *seconds > 0)
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0)),
        deleted_at: mission.deleted_at,
    })
}

/// Every change row of a mission, newest first.
pub(super) async fn all_changes(
    service: &MissionService,
    mission: &Mission,
) -> Result<Vec<crate::db::repos::MissionChangeRow>, ApiError> {
    service.change_rows(mission).await.map_err(failed)
}

/// One stored change, as the admin API reports it.
pub(super) fn change(row: &crate::db::repos::MissionChangeRow) -> MissionChangeSummary {
    MissionChangeSummary {
        kind: MissionChangeKind::parse(&row.kind).unwrap_or(MissionChangeKind::AddContent),
        content_uid: row.content_uid.clone(),
        content_hash: row.content_hash.clone(),
        timestamp: row.timestamp,
        server_time: row.server_time,
        creator_uid: row.creator_uid.clone(),
        details: row
            .detail
            .clone()
            .and_then(|detail| serde_json::from_value::<serde_json::Value>(detail).ok())
            .map(|detail| UidDetails {
                kind: text(&detail, "type"),
                callsign: text(&detail, "callsign"),
                title: text(&detail, "title"),
                iconset_path: text(&detail, "iconsetPath"),
                color: text(&detail, "color"),
                lat: detail.pointer("/location/lat").and_then(|at| at.as_f64()),
                lon: detail.pointer("/location/lon").and_then(|at| at.as_f64()),
            })
            .filter(|details| !details.is_empty()),
    }
}

/// One string field of a cached detail object.
pub(super) fn text(detail: &serde_json::Value, key: &str) -> Option<String> {
    detail
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// The client uids connected to the stream listener right now.
pub(super) fn connected_uids(context: &AppContext) -> Vec<String> {
    match context.has_live().then(|| context.live()) {
        Some(Ok(live)) => live.snapshot().into_iter().map(|peer| peer.uid).collect(),
        _ => Vec::new(),
    }
}

/// The service's role enum, from the API's spelling of it.
pub(super) fn role_of(role: MissionRoleKind) -> Role {
    match role {
        MissionRoleKind::Owner => Role::Owner,
        MissionRoleKind::Subscriber => Role::Subscriber,
        MissionRoleKind::ReadonlySubscriber => Role::ReadonlySubscriber,
    }
}

/// A read or write that failed, as a `500` with the detail logged.
pub(super) fn failed(err: crate::marti::MartiError) -> ApiError {
    warn!(error = %err, "A mission administration request failed.");

    ApiError::internal()
}

/// Records an administrative change, without failing the request if it cannot.
pub(super) async fn record(
    context: &AppContext,
    caller: &Administrative,
    action: &'static str,
    subject: &str,
) {
    let entry = AuditEntry::new(AuditCategory::Administration, action, AuditOutcome::Success)
        .subject(subject)
        .actor(&caller.user.username);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a mission change in the audit log.");
        context.session().record_human_error(&err);
    }
}
