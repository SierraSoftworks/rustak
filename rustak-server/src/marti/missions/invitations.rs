//! `{n}/invite/**` and the two invitation listings.
//!
//! An invitation says who may join a mission they cannot otherwise see, and the
//! `t-x-m-i` it sends is what makes the invitation arrive on a client that is
//! connected right now. The listing is what makes it arrive on one that is not.
//!
//! # `GET /missions/invitations?clientUid=` is never a `404`
//!
//! CloudTAK fetches it in parallel with the mission list and treats a failure
//! as a failure of the whole page, so a caller with no invitations gets an
//! empty array rather than anything that looks like an error.

use actix_web::web;
use serde::{Deserialize, Serialize};

use crate::marti::{CiQuery, MissionRef, response, response::kind};
use crate::missions::MissionService;
use crate::missions::invitations::MissionInvitation;
use crate::missions::model::Mission;
use crate::missions::roles::{Permission, Role};
use crate::prelude::*;

use super::super::error::{MartiError, MartiResult};
use super::{MissionCtx, allowed};

/// One invitation, as the wire spells it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionInvitationJson {
    #[serde(rename = "missionName")]
    pub mission_name: String,
    pub invitee: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "creatorUid", skip_serializing_if = "Option::is_none")]
    pub creator_uid: Option<String>,
    #[serde(rename = "createTime")]
    pub create_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub role: crate::missions::MissionRoleJson,
    #[serde(rename = "missionId")]
    pub mission_id: i64,
    #[serde(rename = "missionGuid")]
    pub mission_guid: String,
}

/// The wire shape of one invitation against one mission.
pub fn render(mission: &Mission, invitation: &MissionInvitation) -> MissionInvitationJson {
    MissionInvitationJson {
        mission_name: mission.name.clone(),
        invitee: invitation.invitee.clone(),
        kind: invitation.kind.clone(),
        creator_uid: invitation.creator_uid.clone(),
        create_time: crate::marti::time::cot_date(invitation.create_time),
        token: invitation.token.clone(),
        role: crate::missions::role_json(invitation.role),
        mission_id: invitation.mission_id,
        mission_guid: mission.guid.to_string(),
    }
}

/// One entry of the bulk `POST {n}/invite` body.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkInvite {
    #[serde(rename = "type")]
    pub kind: String,
    pub invitee: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// `PUT {n}/invite/{type}/{invitee}` — invite somebody.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`, and
/// [`MartiError::InvalidRequest`] for a type outside the five TAK defines.
#[instrument("marti.missions.invite", skip_all)]
pub async fn invite(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String, String)>,
    query: CiQuery,
) -> MartiResult {
    let (_, kind, invitee) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;
    let creator = query.get("creatorUid").map(ToOwned::to_owned);
    let role = parse_role(query.get("role"))?;

    let invitation = ctx
        .service
        .invite(&mission, &kind, &invitee, creator.as_deref(), role)
        .await?;

    announce(&ctx.service, &mission, &invitation, creator.as_deref());

    Ok(response::status::<()>(
        actix_web::http::StatusCode::OK,
        kind::MISSION_INVITATION,
        None,
    ))
}

/// `DELETE {n}/invite/{type}/{invitee}` — withdraw one.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_WRITE`.
#[instrument("marti.missions.uninvite", skip_all)]
pub async fn uninvite(
    ctx: MissionCtx,
    reference: MissionRef,
    path: web::Path<(String, String, String)>,
) -> MartiResult {
    let (_, kind, invitee) = path.into_inner();
    let mission = writable(&ctx, &reference).await?;

    ctx.service.uninvite(&mission, &kind, &invitee).await?;

    Ok(response::status::<()>(
        actix_web::http::StatusCode::OK,
        kind::MISSION_INVITATION,
        None,
    ))
}

/// `POST {n}/invite` — invite several at once.
///
/// # Errors
///
/// As [`invite`].
#[instrument("marti.missions.invite_bulk", skip_all)]
pub async fn invite_bulk(
    ctx: MissionCtx,
    reference: MissionRef,
    query: CiQuery,
    body: web::Json<Vec<BulkInvite>>,
) -> MartiResult {
    let mission = writable(&ctx, &reference).await?;
    let creator = query.get("creatorUid").map(ToOwned::to_owned);

    for entry in body.into_inner() {
        let role = parse_role(entry.role.as_deref())?;
        let invitation = ctx
            .service
            .invite(
                &mission,
                &entry.kind,
                &entry.invitee,
                creator.as_deref(),
                role,
            )
            .await?;

        announce(&ctx.service, &mission, &invitation, creator.as_deref());
    }

    Ok(response::status::<()>(
        actix_web::http::StatusCode::OK,
        kind::MISSION_INVITATION,
        None,
    ))
}

/// `GET {n}/invitations` — everything standing against one mission.
///
/// # Errors
///
/// [`MartiError::Forbidden`] without `MISSION_READ`.
#[instrument("marti.missions.invitations", skip_all)]
pub async fn listing(ctx: MissionCtx, reference: MissionRef) -> MartiResult {
    let mission = allowed(&ctx, &reference, Permission::Read).await?;
    let rendered: Vec<MissionInvitationJson> = ctx
        .service
        .invitations(&mission)
        .await?
        .iter()
        .map(|invitation| render(&mission, invitation))
        .collect();

    Ok(response::ok(kind::MISSION_INVITATION, rendered))
}

/// `GET /missions/invitations?clientUid=` — everything the caller matches.
///
/// # Errors
///
/// A system error if a read fails. Never a `404`.
#[instrument("marti.missions.invitations_for", skip_all)]
pub async fn for_client(ctx: MissionCtx, query: CiQuery) -> MartiResult {
    let target = ctx
        .service
        .invite_target(&ctx.who, query.get("clientUid"))
        .await?;

    let rendered: Vec<MissionInvitationJson> = ctx
        .service
        .invitations_matching(&target)
        .await?
        .iter()
        .map(|(mission, invitation)| render(mission, invitation))
        .collect();

    Ok(response::ok(kind::MISSION_INVITATION, rendered))
}

/// `GET /missions/all/invitations?clientUid=` — the same, as names only.
///
/// # Errors
///
/// A system error if a read fails.
#[instrument("marti.missions.invitations_all", skip_all)]
pub async fn all(ctx: MissionCtx, query: CiQuery) -> MartiResult {
    let target = ctx
        .service
        .invite_target(&ctx.who, query.get("clientUid"))
        .await?;

    let names: Vec<String> = ctx
        .service
        .invitations_matching(&target)
        .await?
        .into_iter()
        .map(|(mission, _)| mission.name)
        .collect();

    Ok(response::ok(kind::MISSION_INVITATION, names))
}

/// Pushes the `t-x-m-i` an invitation is worth, to whoever is connected.
fn announce(
    service: &MissionService,
    mission: &Mission,
    invitation: &MissionInvitation,
    creator_uid: Option<&str>,
) {
    let Some(token) = invitation.token.clone() else {
        return;
    };

    let uids = service.invite_recipients(&invitation.kind, &invitation.invitee);

    service.notify_invited(mission, creator_uid, &token, invitation.role, uids);
}

/// The mission, once the caller is known to be allowed to write to it.
async fn writable(ctx: &MissionCtx, reference: &MissionRef) -> Result<Mission, MartiError> {
    allowed(ctx, reference, Permission::Write).await
}

/// The role an invitation grants, defaulting to `MISSION_SUBSCRIBER`.
fn parse_role(requested: Option<&str>) -> Result<Role, MartiError> {
    let Some(requested) = requested.filter(|value| !value.is_empty()) else {
        return Ok(Role::Subscriber);
    };

    Role::parse(requested)
        .ok_or_else(|| MartiError::InvalidRequest(format!("{requested} is not a mission role")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_role_is_the_subscriber_default() {
        assert_eq!(parse_role(None).unwrap(), Role::Subscriber);
        assert_eq!(parse_role(Some("")).unwrap(), Role::Subscriber);
        assert_eq!(
            parse_role(Some("MISSION_READONLY_SUBSCRIBER")).unwrap(),
            Role::ReadonlySubscriber
        );
        assert!(parse_role(Some("MISSION_ADMIN")).is_err());
    }

    #[test]
    fn an_invitation_renders_the_field_names_the_wire_uses() {
        let mission = Mission {
            id: 3,
            guid: uuid::Uuid::nil(),
            name: "Kettle".to_string(),
            description: String::new(),
            chat_room: None,
            base_layer: None,
            bbox: None,
            bounding_polygon: Vec::new(),
            path: None,
            classification: None,
            tool: "public".to_string(),
            keywords: Vec::new(),
            creator_uid: None,
            create_time: chrono::DateTime::UNIX_EPOCH,
            last_edited: None,
            default_role: Role::Subscriber,
            invite_only: true,
            password_hash: None,
            expiration: None,
            groups: Vec::new(),
            parent_id: None,
            deleted_at: None,
        };
        let invitation = MissionInvitation {
            id: 1,
            mission_id: 3,
            kind: "clientUid".to_string(),
            invitee: "ANDROID-1".to_string(),
            creator_uid: Some("ANDROID-9".to_string()),
            role: Role::Subscriber,
            create_time: chrono::DateTime::UNIX_EPOCH,
            token: Some("eyJ0".to_string()),
        };

        let json = serde_json::to_value(render(&mission, &invitation)).unwrap();

        assert_eq!(json["missionName"], "Kettle");
        assert_eq!(json["type"], "clientUid");
        assert_eq!(json["missionId"], 3);
        assert_eq!(json["token"], "eyJ0");
        assert_eq!(json["role"]["type"], "MISSION_SUBSCRIBER");
    }
}
