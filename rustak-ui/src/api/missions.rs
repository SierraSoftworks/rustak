//! Data Sync missions, as an operator sees them.
//!
//! Not the Marti surface a TAK client talks to: this listing hides nothing, so
//! an invite-only or password-protected mission still appears — somebody
//! asking what is on their server is asking about all of it. The two
//! operations here that a client cannot perform are taking somebody's
//! subscription away and deleting a mission they do not own.
//!
//! # Addressed by guid, never by name
//!
//! A mission may be renamed and a name may itself be a bare UUID, so every
//! route below addresses the immutable identifier the listing already carries.

use rustak_api::{
    MissionChangeSummary, MissionDetail, MissionGuid, MissionRoleKind, MissionSummary,
};

use crate::api::download::{Download, get_download};
use crate::api::{ApiError, delete_empty, get_json, put_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every mission, including the ones a client would not be shown.
pub async fn list() -> Result<Vec<MissionSummary>, ApiError> {
    demo!(Ok(fixtures::missions()));

    get_json("/missions").await
}

/// One mission, with its subscriptions and its layer tree.
pub async fn get(guid: &MissionGuid) -> Result<MissionDetail, ApiError> {
    demo!(fixtures::mission(guid));

    get_json(&format!("/missions/{}", urlencode(&guid.to_string()))).await
}

/// The change log.
///
/// `squashed` asks for one entry per piece of content rather than one per
/// event, which is what a client syncing from scratch receives — so the toggle
/// is the difference between "what happened" and "what a device would be told
/// if it asked now".
pub async fn changes(
    guid: &MissionGuid,
    squashed: bool,
) -> Result<Vec<MissionChangeSummary>, ApiError> {
    demo!(fixtures::mission_changes(guid, squashed));

    get_json(&format!(
        "/missions/{}/changes?squashed={squashed}",
        urlencode(&guid.to_string())
    ))
    .await
}

/// Changes what one subscribed device may do.
///
/// The server answers with the number of rows it changed; nothing here needs
/// it, because a subscription that was not there is a `404` rather than a zero.
pub async fn set_role(
    guid: &MissionGuid,
    client_uid: &str,
    role: MissionRoleKind,
) -> Result<(), ApiError> {
    demo!(fixtures::set_mission_role(guid, client_uid, role));

    let _: serde_json::Value = put_json(
        &format!(
            "/missions/{}/subscriptions/{}/role",
            urlencode(&guid.to_string()),
            urlencode(client_uid)
        ),
        &serde_json::json!({ "role": role }),
    )
    .await?;

    Ok(())
}

/// Takes a device's subscription away.
///
/// It keeps whatever it has already synced — this stops it being sent more,
/// rather than reaching into it.
pub async fn unsubscribe(guid: &MissionGuid, client_uid: &str) -> Result<(), ApiError> {
    demo!(fixtures::unsubscribe_mission(guid, client_uid));

    delete_empty(&format!(
        "/missions/{}/subscriptions/{}",
        urlencode(&guid.to_string()),
        urlencode(client_uid)
    ))
    .await
}

/// Deletes a mission.
///
/// `deep` also removes the content that was only ever attached to it. Without
/// it the files stay in the enterprise sync store, where anything else that
/// referenced them still finds them.
pub async fn remove(guid: &MissionGuid, deep: bool) -> Result<(), ApiError> {
    demo!(fixtures::delete_mission(guid));

    delete_empty(&format!(
        "/missions/{}?deep={deep}",
        urlencode(&guid.to_string())
    ))
    .await
}

/// The mission archive: a Mission Package of everything in it.
pub async fn archive(guid: &MissionGuid) -> Result<Download, ApiError> {
    demo!(fixtures::mission_archive(guid));

    get_download(
        &format!("/missions/{}/archive", urlencode(&guid.to_string())),
        "mission.zip",
    )
    .await
}
