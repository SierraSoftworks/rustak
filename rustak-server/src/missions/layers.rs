//! The layer tree a mission's contents are filed under.
//!
//! A layer is a folder: it has a uid, a name, a type, a parent and a position
//! among its siblings, and map items and resources are filed under one by uid.
//! `GET …/layers` renders the whole tree with the items hanging off it, which
//! is what ATAK's Data Sync view draws and what CloudTAK's ETL writes into.
//!
//! # Deleting a layer does not delete what was in it
//!
//! The items are **unfiled**, not removed: `layer_uid` goes back to `NULL` and
//! the item stays in the mission. A client that deletes a folder expects its
//! markers to move to the root, and TAK Server does the same — removing them
//! would make an accidental folder delete lose an exercise's work.
//!
//! # `mission_layers` is snake_case on purpose
//!
//! It is the one key in the whole mission JSON surface that is not camelCase.
//! Renaming it would be tidier and would break every client that reads a tree.

use serde::{Deserialize, Serialize};

use crate::db::row::Timestamp;
use crate::marti::MartiError;

use super::model::Mission;
use super::service::MissionService;

/// The five layer types TAK defines.
pub const LAYER_TYPES: &[&str] = &["GROUP", "UID", "CONTENTS", "MAPLAYER", "ITEM"];

/// The type a map layer is stored under.
pub const MAP_LAYER: &str = "MAPLAYER";

/// Every column [`MissionLayer::from_row`] reads, in order.
const COLUMNS: &str = "uid, name, type, parent_uid, position, creator_uid, data";

/// One stored layer.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionLayer {
    pub uid: String,
    pub name: Option<String>,
    /// One of [`LAYER_TYPES`].
    pub kind: String,
    pub parent_uid: Option<String>,
    pub position: i64,
    pub creator_uid: Option<String>,
    /// A map layer's body, stored exactly as it was received.
    pub data: Option<serde_json::Value>,
}

impl MissionLayer {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            uid: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
            parent_uid: row.get(3)?,
            position: row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
            creator_uid: row.get(5)?,
            data: crate::db::row::opt_json_col(row, 6)?,
        })
    }

    /// The notice payload a `t-x-m-c-h` carries for this layer.
    pub fn to_notice(&self) -> crate::stream::MissionLayerXml {
        crate::stream::MissionLayerXml {
            uid: self.uid.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            parent_uid: self.parent_uid.clone(),
        }
    }
}

/// What a layer create was asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewLayer {
    /// Absent means a fresh uuid.
    pub uid: Option<String>,
    pub name: Option<String>,
    pub kind: String,
    pub parent_uid: Option<String>,
    /// The sibling to insert after; absent or empty means append.
    pub after_uid: Option<String>,
    pub creator_uid: Option<String>,
    pub data: Option<serde_json::Value>,
}

/// One node of the rendered tree.
///
/// `@JsonInclude(NON_EMPTY)`: an empty array is omitted as well as a null, so a
/// leaf carries only the four fields it actually has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MissionLayerJson {
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "parentUid", default, skip_serializing_if = "Option::is_none")]
    pub parent_uid: Option<String>,
    /// The one deliberately snake_case key in the mission API.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mission_layers: Vec<MissionLayerJson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uids: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contents: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maplayers: Vec<serde_json::Value>,
}

impl MissionService {
    /// Every layer of a mission, in tree order within each parent.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn layers(&self, mission: &Mission) -> Result<Vec<MissionLayer>, MartiError> {
        let mission_id = mission.id;

        Ok(self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_layers WHERE mission_id = ?1 \
                     ORDER BY COALESCE(position, 0), id"
                ))?;

                statement
                    .query_map(rusqlite::params![mission_id], MissionLayer::from_row)?
                    .collect()
            })
            .await?)
    }

    /// One layer by uid.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn layer(
        &self,
        mission: &Mission,
        uid: &str,
    ) -> Result<Option<MissionLayer>, MartiError> {
        Ok(self
            .layers(mission)
            .await?
            .into_iter()
            .find(|layer| layer.uid == uid))
    }

    /// The map items filed under a mission, with the layer each sits in.
    ///
    /// Read here rather than in the route file so that the tree renderer does
    /// not have to know which repository holds what.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn layer_items(
        &self,
        mission: &Mission,
    ) -> Result<Vec<crate::db::repos::MissionUidRow>, MartiError> {
        Ok(self.db().mission_contents().uids(mission.id).await?)
    }

    /// How many resources are filed under a mission.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn filed_contents_count(&self, mission: &Mission) -> Result<usize, MartiError> {
        Ok(self
            .db()
            .mission_contents()
            .contents(mission.id)
            .await?
            .len())
    }

    /// Every change row of a mission, newest first.
    ///
    /// The window is open at both ends: an operator asking what happened to a
    /// mission is asking about all of it, not about the last day of it.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn change_rows(
        &self,
        mission: &Mission,
    ) -> Result<Vec<crate::db::repos::MissionChangeRow>, MartiError> {
        Ok(self
            .db()
            .mission_changes()
            .window(
                mission.id,
                chrono::DateTime::UNIX_EPOCH,
                chrono::Utc::now() + chrono::Duration::days(1),
            )
            .await?)
    }

    /// Creates a layer, appending it or inserting it after a sibling.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a type outside [`LAYER_TYPES`], and a
    /// system error if the write fails.
    pub async fn add_layer(
        &self,
        mission: &Mission,
        new: NewLayer,
    ) -> Result<MissionLayer, MartiError> {
        if !LAYER_TYPES.contains(&new.kind.as_str()) {
            return Err(MartiError::InvalidRequest(format!(
                "layer type must be one of {}",
                LAYER_TYPES.join(", ")
            )));
        }

        let existing = self.layers(mission).await?;
        let position = position_for(
            &existing,
            new.parent_uid.as_deref(),
            new.after_uid.as_deref(),
        );
        let uid = new
            .uid
            .filter(|uid| !uid.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let data = match &new.data {
            Some(value) => Some(serde_json::to_string(value)?),
            None => None,
        };
        let mission_id = mission.id;
        let (name, kind, parent) = (new.name.clone(), new.kind.clone(), new.parent_uid.clone());
        let creator = new.creator_uid.clone();
        let stored = uid.clone();

        self.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mission_layers \
                       (mission_id, uid, name, type, parent_uid, after_uid, position, creator_uid, \
                        data, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, ?9) \
                     ON CONFLICT (mission_id, uid) DO UPDATE SET \
                       name = excluded.name, type = excluded.type, \
                       parent_uid = excluded.parent_uid, position = excluded.position, \
                       data = excluded.data, updated_at = excluded.updated_at",
                    rusqlite::params![
                        mission_id,
                        stored,
                        name,
                        kind,
                        parent,
                        position,
                        creator,
                        data,
                        Timestamp::now(),
                    ],
                )
            })
            .await?;

        self.layer(mission, &uid)
            .await?
            .ok_or_else(|| MartiError::Internal("the layer just written is not there".to_string()))
    }

    /// Renames a layer, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn rename_layer(
        &self,
        mission: &Mission,
        uid: &str,
        name: &str,
    ) -> Result<bool, MartiError> {
        let (mission_id, uid, name) = (mission.id, uid.to_string(), name.to_string());

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE mission_layers SET name = ?3, updated_at = ?4 \
                     WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid, name, Timestamp::now()],
                )? > 0)
            })
            .await?)
    }

    /// Moves a layer under a new parent, or to a new position among siblings.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn move_layer(
        &self,
        mission: &Mission,
        uid: &str,
        parent_uid: Option<&str>,
        after_uid: Option<&str>,
    ) -> Result<bool, MartiError> {
        let existing = self.layers(mission).await?;

        let Some(layer) = existing.iter().find(|layer| layer.uid == uid) else {
            return Ok(false);
        };

        let parent = parent_uid
            .map(ToOwned::to_owned)
            .or_else(|| layer.parent_uid.clone());
        let position = position_for(&existing, parent.as_deref(), after_uid);
        let (mission_id, uid) = (mission.id, uid.to_string());

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE mission_layers SET parent_uid = ?3, position = ?4, updated_at = ?5 \
                     WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid, parent, position, Timestamp::now()],
                )? > 0)
            })
            .await?)
    }

    /// Deletes layers and their descendants, unfiling what was in them.
    ///
    /// # Errors
    ///
    /// A system error if a write fails.
    pub async fn delete_layers(
        &self,
        mission: &Mission,
        uids: &[String],
    ) -> Result<usize, MartiError> {
        let existing = self.layers(mission).await?;
        let doomed = with_descendants(&existing, uids);

        if doomed.is_empty() {
            return Ok(0);
        }

        let mission_id = mission.id;

        Ok(self
            .db()
            .write(move |tx| {
                let mut removed = 0;

                for uid in &doomed {
                    // The items come first: a row whose layer has gone would
                    // otherwise point at nothing.
                    tx.execute(
                        "UPDATE mission_uids SET layer_uid = NULL, position = NULL \
                         WHERE mission_id = ?1 AND layer_uid = ?2",
                        rusqlite::params![mission_id, uid],
                    )?;
                    tx.execute(
                        "UPDATE mission_contents SET layer_uid = NULL, position = NULL \
                         WHERE mission_id = ?1 AND layer_uid = ?2",
                        rusqlite::params![mission_id, uid],
                    )?;
                    removed += tx.execute(
                        "DELETE FROM mission_layers WHERE mission_id = ?1 AND uid = ?2",
                        rusqlite::params![mission_id, uid],
                    )?;
                }

                Ok(removed)
            })
            .await?)
    }
}

/// The position a new or moved layer takes among its siblings.
///
/// Appending is the default; `after_uid` inserts directly behind that sibling
/// by taking a position one past it, which is enough while positions are only
/// ever read in order.
fn position_for(existing: &[MissionLayer], parent: Option<&str>, after: Option<&str>) -> i64 {
    let siblings: Vec<&MissionLayer> = existing
        .iter()
        .filter(|layer| layer.parent_uid.as_deref() == parent)
        .collect();

    if let Some(after) = after.filter(|uid| !uid.trim().is_empty())
        && let Some(sibling) = siblings.iter().find(|layer| layer.uid == after)
    {
        return sibling.position + 1;
    }

    siblings
        .iter()
        .map(|layer| layer.position)
        .max()
        .map_or(0, |highest| highest + 1)
}

/// The named layers plus everything beneath them.
fn with_descendants(existing: &[MissionLayer], roots: &[String]) -> Vec<String> {
    let mut doomed: Vec<String> = existing
        .iter()
        .filter(|layer| roots.contains(&layer.uid))
        .map(|layer| layer.uid.clone())
        .collect();

    let mut index = 0;

    while index < doomed.len() {
        let parent = doomed[index].clone();

        for layer in existing {
            if layer.parent_uid.as_deref() == Some(parent.as_str()) && !doomed.contains(&layer.uid)
            {
                doomed.push(layer.uid.clone());
            }
        }

        index += 1;
    }

    doomed
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    use super::super::roles::Role;
    use super::*;

    async fn fixture() -> (AppContext, MissionService, Mission) {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let service = MissionService::new(context.clone());
        let row = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        (context, service, Mission::from_row(row))
    }

    fn layer(uid: &str, parent: Option<&str>) -> NewLayer {
        NewLayer {
            uid: Some(uid.to_string()),
            name: Some(uid.to_uppercase()),
            kind: "UID".to_string(),
            parent_uid: parent.map(ToOwned::to_owned),
            ..NewLayer::default()
        }
    }

    #[tokio::test]
    async fn layers_are_appended_in_the_order_they_are_created() {
        let (_context, service, mission) = fixture().await;

        for uid in ["one", "two", "three"] {
            service.add_layer(&mission, layer(uid, None)).await.unwrap();
        }

        let uids: Vec<String> = service
            .layers(&mission)
            .await
            .unwrap()
            .into_iter()
            .map(|layer| layer.uid)
            .collect();

        assert_eq!(uids, ["one", "two", "three"]);
    }

    #[tokio::test]
    async fn a_layer_created_after_a_sibling_lands_behind_it() {
        let (_context, service, mission) = fixture().await;
        service
            .add_layer(&mission, layer("one", None))
            .await
            .unwrap();
        service
            .add_layer(&mission, layer("three", None))
            .await
            .unwrap();

        service
            .add_layer(
                &mission,
                NewLayer {
                    after_uid: Some("one".to_string()),
                    ..layer("two", None)
                },
            )
            .await
            .unwrap();

        let positions: Vec<i64> = service
            .layers(&mission)
            .await
            .unwrap()
            .into_iter()
            .map(|layer| layer.position)
            .collect();

        assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[tokio::test]
    async fn an_unknown_layer_type_is_refused() {
        let (_context, service, mission) = fixture().await;

        let refused = service
            .add_layer(
                &mission,
                NewLayer {
                    kind: "FOLDER".to_string(),
                    ..layer("one", None)
                },
            )
            .await;

        assert!(matches!(refused, Err(MartiError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn deleting_a_layer_takes_its_children_and_unfiles_its_items() {
        let (context, service, mission) = fixture().await;
        service
            .add_layer(&mission, layer("root", None))
            .await
            .unwrap();
        service
            .add_layer(&mission, layer("child", Some("root")))
            .await
            .unwrap();
        service
            .add_layer(&mission, layer("other", None))
            .await
            .unwrap();

        context
            .db()
            .mission_contents()
            .upsert_uid(crate::db::repos::MissionUidRow {
                layer_uid: Some("child".to_string()),
                ..crate::db::repos::MissionUidRow::new(mission.id, "UID-A", chrono::Utc::now())
            })
            .await
            .unwrap();

        let removed = service
            .delete_layers(&mission, &["root".to_string()])
            .await
            .unwrap();

        assert_eq!(removed, 2, "the child went with its parent");
        let left = service.layers(&mission).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].uid, "other");

        let items = context
            .db()
            .mission_contents()
            .uids(mission.id)
            .await
            .unwrap();
        assert_eq!(items.len(), 1, "the item is still in the mission");
        assert_eq!(items[0].layer_uid, None, "but it is unfiled");
    }

    #[tokio::test]
    async fn renaming_and_moving_report_whether_the_layer_was_there() {
        let (_context, service, mission) = fixture().await;
        service
            .add_layer(&mission, layer("root", None))
            .await
            .unwrap();
        service
            .add_layer(&mission, layer("leaf", None))
            .await
            .unwrap();

        assert!(
            service
                .rename_layer(&mission, "leaf", "Markers")
                .await
                .unwrap()
        );
        assert!(
            !service
                .rename_layer(&mission, "gone", "Markers")
                .await
                .unwrap()
        );
        assert!(
            service
                .move_layer(&mission, "leaf", Some("root"), None)
                .await
                .unwrap()
        );
        assert!(
            !service
                .move_layer(&mission, "gone", Some("root"), None)
                .await
                .unwrap()
        );

        let leaf = service.layer(&mission, "leaf").await.unwrap().unwrap();
        assert_eq!(leaf.name.as_deref(), Some("Markers"));
        assert_eq!(leaf.parent_uid.as_deref(), Some("root"));
        assert_eq!(leaf.to_notice().parent_uid.as_deref(), Some("root"));
    }

    #[tokio::test]
    async fn deleting_a_layer_that_is_not_there_removes_nothing() {
        let (_context, service, mission) = fixture().await;

        assert_eq!(
            service
                .delete_layers(&mission, &["gone".to_string()])
                .await
                .unwrap(),
            0
        );
    }
}
