//! Creating, updating, deleting and copying a mission.
//!
//! One `PUT` or `POST` does both halves of the first two: TAK Server's mission
//! API has no separate create route, and a client that sends the same request
//! twice means "make sure this exists and looks like this". The status is what
//! tells the two apart — `201` with a token, `200` without — and CloudTAK reads
//! a missing token on a create as a failure, so the distinction is load-bearing
//! rather than cosmetic.
//!
//! # What an update may change, and who may change it
//!
//! The descriptive fields need `MISSION_WRITE`. The fields that decide *who can
//! see or do what* — the channels, the password, the default role, the
//! invite-only flag and the expiry — need `MISSION_OWNER` or an administrator,
//! because a subscriber who could widen a mission's channels could share
//! somebody else's work with a channel they chose.

use chrono::Utc;

use crate::db::repos::{MissionPatch, NewChange, NewMission};
use crate::marti::{MartiError, MartiPrincipal};

use super::model::{CopyParams, Mission, MissionParams, Outcome, validate_name};
use super::roles::{Permission, Role, require};
use super::service::{
    CREATE_MISSION, DELETE_MISSION, MissionService, duplicate_name, expiration_of, hash_password,
    resolve_groups,
};

impl MissionService {
    /// Creates a mission, or updates the one that already has the name.
    ///
    /// # Errors
    ///
    /// [`MartiError::Validation`] for a name we will not accept,
    /// [`MartiError::Unauthorized`] for an anonymous create and
    /// [`MartiError::Forbidden`] for an update the caller's role does not
    /// carry.
    pub async fn create_or_update(
        &self,
        who: &MartiPrincipal,
        claims: Option<&crate::auth::MissionClaims>,
        name: &str,
        params: MissionParams,
    ) -> Result<Outcome, MartiError> {
        let name = validate_name(name)?;

        match self.by_name(&name).await? {
            Some(mission) if !mission.is_deleted() => {
                self.update(who, claims, mission, params).await
            }
            _ => self.create(who, name, params).await,
        }
    }

    /// Creates a mission and its owner's subscription.
    async fn create(
        &self,
        who: &MartiPrincipal,
        name: String,
        params: MissionParams,
    ) -> Result<Outcome, MartiError> {
        let resolved = who.require()?;
        let viewer = self.viewer(who).await?;
        let groups = resolve_groups(&viewer, params.groups.clone())?;
        let password_hash = match params.password.as_deref() {
            Some(password) if !password.is_empty() => Some(hash_password(password).await?),
            _ => None,
        };

        let row = self
            .db()
            .missions()
            .create(NewMission {
                description: params.description.clone().unwrap_or_default(),
                chat_room: params.chat_room.clone(),
                base_layer: params.base_layer.clone(),
                bbox: params.bbox.clone(),
                bounding_polygon: params.bounding_polygon.clone().unwrap_or_default(),
                path: params.path.clone(),
                classification: params.classification.clone(),
                tool: params.tool.clone().unwrap_or_else(|| "public".to_string()),
                keywords: params.keywords.clone().unwrap_or_default(),
                creator_uid: params.creator_uid.clone(),
                invite_only: params.invite_only.unwrap_or(false),
                password_hash,
                expiration: expiration_of(params.expiration),
                groups,
                ..NewMission::new(name, params.default_role.unwrap_or_default().as_str())
            })
            .await
            .map_err(duplicate_name)?;

        let mission = Mission::from_row(row);

        self.db()
            .mission_changes()
            .record(
                NewChange::new(mission.id, CREATE_MISSION, mission.create_time)
                    .by(params.creator_uid.as_deref()),
            )
            .await?;

        let owner_uid = params
            .creator_uid
            .clone()
            .or(params.device_uid.clone())
            .unwrap_or_else(|| resolved.user.username.to_string());
        let token = self
            .subscribe_owner(&mission, &owner_uid, who.username())
            .await?;

        // Announced before the package is imported, so a client that hears
        // about the mission and fetches it sees the contents arrive rather than
        // an empty mission it has already read.
        self.notify_created(&mission, params.creator_uid.as_deref());

        if let Some(package) = params.package {
            self.import_package(&mission, package, params.creator_uid.as_deref())
                .await?;
        }

        Ok(Outcome::Created {
            mission,
            token,
            owner_role: Role::Owner,
        })
    }

    /// Applies an update to a mission that already exists.
    async fn update(
        &self,
        who: &MartiPrincipal,
        claims: Option<&crate::auth::MissionClaims>,
        mission: Mission,
        params: MissionParams,
    ) -> Result<Outcome, MartiError> {
        let role = self.role_for_request(&mission, who, claims).await?;
        let privileged = who.is_admin() || role.is_some_and(|role| role.kind == Role::Owner);
        let mut patch = MissionPatch {
            description: params.description.clone(),
            chat_room: params.chat_room.clone().map(Some),
            base_layer: params.base_layer.clone().map(Some),
            bbox: params.bbox.clone().map(Some),
            bounding_polygon: params.bounding_polygon.clone(),
            path: params.path.clone().map(Some),
            classification: params.classification.clone().map(Some),
            tool: params.tool.clone(),
            keywords: params.keywords.clone(),
            ..MissionPatch::default()
        };

        if !patch.is_empty() {
            require(role, Permission::Write)?;
        }

        if params.groups.is_some()
            || params.password.is_some()
            || params.default_role.is_some()
            || params.invite_only.is_some()
            || params.expiration.is_some()
        {
            if !privileged {
                return Err(MartiError::Forbidden(
                    "changing a mission's access needs MISSION_OWNER".to_string(),
                ));
            }

            let viewer = self.viewer(who).await?;

            patch.groups = params
                .groups
                .clone()
                .map(|groups| resolve_groups(&viewer, Some(groups)))
                .transpose()?;
            patch.default_role = params.default_role.map(|role| role.as_str().to_string());
            patch.invite_only = params.invite_only;
            patch.expiration = params.expiration.map(|value| expiration_of(Some(value)));

            if let Some(password) = params.password.as_deref() {
                patch.password_hash = Some(match password.is_empty() {
                    true => None,
                    false => Some(hash_password(password).await?),
                });
            }
        }

        let updated = self
            .db()
            .missions()
            .update(mission.id, patch)
            .await?
            .ok_or_else(|| MartiError::NotFound(format!("Mission {}", mission.name)))?;

        let updated = Mission::from_row(updated);
        self.notify_broadcast(
            &updated,
            crate::stream::ChangeKind::Metadata,
            params.creator_uid.as_deref(),
        );

        Ok(Outcome::Updated(updated))
    }

    /// Soft-deletes a mission and records that it happened.
    ///
    /// # Errors
    ///
    /// [`MartiError::Gone`] when it was already deleted.
    pub async fn delete(
        &self,
        mission: &Mission,
        creator_uid: Option<&str>,
        deep: bool,
    ) -> Result<Mission, MartiError> {
        // Archived before the row is retired, so that deleting a mission is
        // recoverable: the zip is an ordinary Mission Package and importing it
        // rebuilds what was lost.
        self.store_archive(mission).await;

        let now = Utc::now();
        let row = self
            .db()
            .missions()
            .soft_delete(mission.id, now)
            .await?
            .ok_or_else(|| MartiError::Gone(format!("Mission {} was deleted", mission.name)))?;

        self.db()
            .mission_changes()
            .record(NewChange::new(mission.id, DELETE_MISSION, now).by(creator_uid))
            .await?;

        if deep {
            self.purge_contents(mission).await?;
        }

        let deleted = Mission::from_row(row);
        self.notify_deleted(&deleted, creator_uid);

        Ok(deleted)
    }

    /// Clones a mission's metadata and everything filed under it.
    ///
    /// # Errors
    ///
    /// [`MartiError::Validation`] for a copy name we will not accept, and
    /// [`MartiError::Duplicate`] when that name is taken.
    pub async fn copy(
        &self,
        who: &MartiPrincipal,
        mission: &Mission,
        params: CopyParams,
    ) -> Result<Mission, MartiError> {
        let name = validate_name(
            &params
                .copy_name
                .clone()
                .unwrap_or_else(|| format!("{} copy", mission.name)),
        )?;
        let password_hash = match params.password.as_deref() {
            Some(password) if !password.is_empty() => Some(hash_password(password).await?),
            _ => None,
        };

        let copied = self
            .create(
                who,
                name,
                MissionParams {
                    creator_uid: params.creator_uid.clone(),
                    description: Some(mission.description.clone()),
                    chat_room: mission.chat_room.clone(),
                    base_layer: mission.base_layer.clone(),
                    bbox: mission.bbox.clone(),
                    bounding_polygon: Some(mission.bounding_polygon.clone()),
                    path: params.copy_path.clone().or_else(|| mission.path.clone()),
                    classification: mission.classification.clone(),
                    tool: Some(mission.tool.clone()),
                    keywords: Some(mission.keywords.clone()),
                    default_role: params.default_role.or(Some(mission.default_role)),
                    expiration: mission.expiration,
                    invite_only: Some(mission.invite_only),
                    groups: Some(mission.groups.clone()),
                    ..MissionParams::default()
                },
            )
            .await?;

        let copy = copied.mission().clone();

        if password_hash.is_some() {
            self.db()
                .missions()
                .update(
                    copy.id,
                    MissionPatch {
                        password_hash: Some(password_hash),
                        ..MissionPatch::default()
                    },
                )
                .await?;
        }

        self.clone_contents(mission, &copy, params.creator_uid.as_deref())
            .await?;

        Ok(copy)
    }
}
