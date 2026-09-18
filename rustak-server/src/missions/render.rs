//! Rendering a mission as the JSON every mission endpoint answers with.
//!
//! One function, because the shape is one shape: the create response, the
//! listing, the single read and the subscription's nested copy are all the same
//! object with different extras attached. [`Render`] is those extras, so that a
//! route says what it is adding rather than assembling the object itself.
//!
//! # Emptied rather than refused
//!
//! A caller with no read role on a mission gets a `403` when they speak
//! `API_VERSION` 2, and the same object with its content arrays emptied when
//! they speak 3 or newer. Newer TAK-ecosystem clients expect the second, and a
//! stripped body says "this mission exists and is not yours" without saying
//! anything about what is in it.

use crate::files;
use crate::marti::{MartiError, time};

use super::dto::{MissionAddJson, MissionJson};
use super::model::Mission;
use super::service::MissionService;

/// What a mission payload should carry beyond the mission itself.
#[derive(Debug, Clone, Default)]
pub struct Render {
    /// The `SUBSCRIPTION` token, on the `201` a create answers with.
    pub token: Option<String>,
    /// The owner's role, on the same `201`.
    pub owner_role: Option<super::roles::Role>,
    /// The change history, when the caller asked with `?changes=true`.
    pub changes: Option<Vec<super::dto::MissionChangeJson>>,
    /// The log entries, when the caller asked with `?logs=true`.
    pub logs: Option<Vec<serde_json::Value>>,
    /// Whether the content arrays are emptied, which is what a caller with no
    /// read role gets when they speak `API_VERSION` 3 or newer.
    pub stripped: bool,
}

impl MissionService {
    /// Renders a mission as the wire shape every mission endpoint emits.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn render(
        &self,
        mission: &Mission,
        extras: Render,
    ) -> Result<MissionJson, MartiError> {
        let (uids, contents) = match extras.stripped {
            true => (Vec::new(), Vec::new()),
            false => (
                self.filed_uids(mission).await?,
                self.filed_contents(mission).await?,
            ),
        };

        let external_data = self
            .external_data(mission)
            .await?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        let map_layers: Vec<serde_json::Value> = self
            .map_layers(mission)
            .await?
            .into_iter()
            .map(|layer| layer.body)
            .collect();
        let feeds = self.feeds(mission).await?;

        Ok(MissionJson {
            name: mission.name.clone(),
            description: mission.description.clone(),
            chat_room: mission.chat_room.clone(),
            base_layer: mission.base_layer.clone(),
            bbox: mission.bbox.clone(),
            bounding_polygon: mission.bounding_polygon.clone(),
            path: mission.path.clone(),
            classification: mission.classification.clone(),
            tool: mission.tool.clone(),
            keywords: mission.keywords.clone(),
            creator_uid: mission.creator_uid.clone(),
            create_time: time::cot_date(mission.create_time),
            last_edited: mission.last_edited.map(time::cot_date),
            expiration: mission.expiration.unwrap_or(-1),
            uids,
            contents,
            groups: mission.effective_groups(),
            // Emitted even when empty rather than omitted: CloudTAK's schema
            // makes all three non-optional arrays, and an absent one fails the
            // whole payload rather than the field.
            external_data,
            map_layers,
            feeds,
            password_protected: mission.is_password_protected(),
            invite_only: mission.invite_only,
            default_role: super::role_json(mission.default_role),
            token: extras.token,
            owner_role: extras.owner_role.map(super::role_json),
            mission_changes: extras.changes,
            logs: extras.logs,
            guid: mission.guid.to_string(),
        })
    }

    /// The map items filed under a mission, in wire order.
    async fn filed_uids(
        &self,
        mission: &Mission,
    ) -> Result<Vec<MissionAddJson<String>>, MartiError> {
        Ok(self
            .db()
            .mission_contents()
            .uids(mission.id)
            .await?
            .into_iter()
            .map(|row| MissionAddJson {
                data: row.uid,
                timestamp: time::cot_date(row.timestamp),
                creator_uid: row.creator_uid,
                keywords: row.keywords,
            })
            .collect())
    }

    /// The resources filed under a mission, in wire order.
    ///
    /// A row whose resource has since been deleted is dropped rather than
    /// rendered as a hole: a client reading `contents[].data.hash` would
    /// otherwise be handed a hash it cannot download.
    async fn filed_contents(
        &self,
        mission: &Mission,
    ) -> Result<Vec<MissionAddJson<files::ResourceJson>>, MartiError> {
        let filed = self.db().mission_contents().contents(mission.id).await?;
        let mut rendered = Vec::with_capacity(filed.len());

        for row in filed {
            if let Some(resource) = self.db().resources().by_id(row.resource_id).await? {
                rendered.push(MissionAddJson {
                    data: files::resource_json(&resource),
                    timestamp: time::cot_date(row.timestamp),
                    creator_uid: row.creator_uid,
                    keywords: row.keywords,
                });
            }
        }

        Ok(rendered)
    }
}
