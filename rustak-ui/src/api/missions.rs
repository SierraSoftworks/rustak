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
use crate::api::{ApiError, Verb, delete_empty, get_json, json_response, put_json, send};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every mission, including the ones a client would not be shown.
pub async fn list() -> Result<Vec<MissionSummary>, ApiError> {
    demo!(Ok(fixtures::missions()));

    get_json("/missions").await
}

/// The status a deleted mission's detail answers with.
const GONE: u16 = 410;

/// One mission, with its subscriptions and its layer tree.
///
/// A deleted one answers `410` **carrying the whole document** rather than the
/// `{"error": …}` shape: the status is the honest answer to "is this mission
/// here?" and the body is the honest answer to "what happened to it?", and an
/// operator who has just followed a link from an audit entry is asking the
/// second. So this route reads the body itself instead of letting the generic
/// client turn the status into [`ApiError::Gone`] — which is the right
/// conversion for the setup wizard and the wrong one here.
pub async fn get(guid: &MissionGuid) -> Result<MissionDetail, ApiError> {
    demo!(fixtures::mission(guid));

    let response = send::<()>(
        Verb::Get,
        &format!("/missions/{}", urlencode(&guid.to_string())),
        None,
    )
    .await?;

    if response.status() == GONE {
        return deleted(response.json::<serde_json::Value>().await.ok());
    }

    json_response(response).await
}

/// The mission a `410` carried, or [`ApiError::Gone`] when it carried nothing
/// we can render.
///
/// A body that is not a mission is not worth guessing at: a server that
/// answered `410` for some other reason, or an intermediary that replaced the
/// body, both land here, and "that is no longer available" is true of each.
fn deleted(body: Option<serde_json::Value>) -> Result<MissionDetail, ApiError> {
    body.and_then(|body| serde_json::from_value::<MissionDetail>(body).ok())
        .ok_or(ApiError::Gone)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A deleted mission exactly as `web/api/missions.rs::get` sends one.
    ///
    /// Written out rather than serialised from a fixture, because what is
    /// being tested is that this *wire shape* is recognised: the summary is
    /// `#[serde(flatten)]`ed into the document, so `name` and `deleted_at` sit
    /// at the top level beside `layers` and `change_count`.
    fn gone_body() -> serde_json::Value {
        serde_json::json!({
            "guid": "f0a19c52-3e64-4b8d-8a17-55c2d9e40b31",
            "name": "Operation Stand Down",
            "tool": "public",
            "create_time": "2026-09-10T08:00:00.000Z",
            "groups": ["__ANON__"],
            "keywords": [],
            "subscriber_count": 0,
            "uid_count": 0,
            "content_count": 0,
            "password_protected": false,
            "invite_only": false,
            "default_role": "MISSION_SUBSCRIBER",
            "deleted_at": "2026-09-17T08:00:00.000Z",
            "subscriptions": [],
            "layers": [],
            "change_count": 4,
        })
    }

    #[test]
    fn a_deleted_mission_is_read_out_of_the_gone_body() {
        // M3-06 made the detail answer `410` with the whole document in it.
        // Turning that into a bare error threw away the only copy of what the
        // operator had followed a link from an audit entry to look at.
        let read = deleted(Some(gone_body())).expect("the body is a mission");

        assert_eq!(read.summary.name, "Operation Stand Down");
        assert_eq!(read.change_count, 4);
        assert!(
            read.summary.deleted_at.is_some(),
            "and it says when the mission went, which is the question being asked",
        );
    }

    #[test]
    fn a_gone_body_that_is_not_a_mission_stays_an_error() {
        assert_eq!(deleted(None), Err(ApiError::Gone));
        assert_eq!(
            deleted(Some(serde_json::json!({ "error": "no" }))),
            Err(ApiError::Gone),
            "the error shape is not a mission and must not be rendered as one",
        );
    }
}
