//! Map layers, external data and data feeds: the three opaque side aggregates.
//!
//! All three hang off a mission, all three are reported as arrays on every
//! `Mission` payload (CloudTAK's schema requires them present even when empty),
//! and none of them means anything to this server. A map layer's body is stored
//! and handed back **verbatim**, so a client that adds a field of its own keeps
//! it; external data is the five fields TAK defines and nothing else; and a
//! data feed is a row we list and never act on.
//!
//! # Why a feed is a no-op rather than a `501`
//!
//! A `501` would be honest and would also stop ATAK's Data Sync screen loading.
//! The endpoints answer `200`, the row is kept so a listing reports what was
//! registered, and nothing consumes it — which is exactly what an operator with
//! no federation configured would observe from a real server anyway.

use serde::{Deserialize, Serialize};

use crate::db::row::Timestamp;
use crate::marti::MartiError;

use super::model::Mission;
use super::service::MissionService;

/// One stored map layer, as the client sent it.
#[derive(Debug, Clone, PartialEq)]
pub struct MapLayer {
    pub uid: String,
    pub name: Option<String>,
    /// The body, exactly as received.
    pub body: serde_json::Value,
    pub creator_uid: Option<String>,
}

/// One external tool's data, reported under `Mission.externalData`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExternalData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(rename = "urlData", default, skip_serializing_if = "Option::is_none")]
    pub url_data: Option<String>,
    #[serde(rename = "urlView", default, skip_serializing_if = "Option::is_none")]
    pub url_view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl MissionService {
    /// Every map layer of a mission.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn map_layers(&self, mission: &Mission) -> Result<Vec<MapLayer>, MartiError> {
        let mission_id = mission.id;

        Ok(self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT uid, name, body, creator_uid FROM map_layers \
                     WHERE mission_id = ?1 ORDER BY id",
                )?;

                statement
                    .query_map(rusqlite::params![mission_id], |row| {
                        Ok(MapLayer {
                            uid: row.get(0)?,
                            name: row.get(1)?,
                            body: crate::db::row::json_col(row, 2)?,
                            creator_uid: row.get(3)?,
                        })
                    })?
                    .collect()
            })
            .await?)
    }

    /// Stores a map layer, minting a uid when the body carries none.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn put_map_layer(
        &self,
        mission: &Mission,
        mut body: serde_json::Value,
        creator_uid: Option<&str>,
    ) -> Result<MapLayer, MartiError> {
        let uid = body
            .get("uid")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|uid| !uid.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Written back into the body as well as the column, so that the copy a
        // client reads back names itself the way ours does.
        if let Some(object) = body.as_object_mut() {
            object.insert("uid".to_string(), serde_json::Value::String(uid.clone()));
        }

        let name = body
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let stored = serde_json::to_string(&body)?;
        let (mission_id, key, creator) =
            (mission.id, uid.clone(), creator_uid.map(ToOwned::to_owned));
        let label = name.clone();

        self.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO map_layers \
                       (mission_id, uid, name, body, creator_uid, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
                     ON CONFLICT (mission_id, uid) DO UPDATE SET \
                       name = excluded.name, body = excluded.body, \
                       updated_at = excluded.updated_at",
                    rusqlite::params![mission_id, key, label, stored, creator, Timestamp::now()],
                )
            })
            .await?;

        Ok(MapLayer {
            uid,
            name,
            body,
            creator_uid: creator_uid.map(ToOwned::to_owned),
        })
    }

    /// Removes a map layer, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn delete_map_layer(&self, mission: &Mission, uid: &str) -> Result<bool, MartiError> {
        let (mission_id, uid) = (mission.id, uid.to_string());

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM map_layers WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid],
                )? > 0)
            })
            .await?)
    }

    /// Every external-data record of a mission.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn external_data(&self, mission: &Mission) -> Result<Vec<ExternalData>, MartiError> {
        let mission_id = mission.id;

        Ok(self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT uid, name, tool, url_data, url_view, notes FROM mission_external_data \
                     WHERE mission_id = ?1 ORDER BY id",
                )?;

                statement
                    .query_map(rusqlite::params![mission_id], |row| {
                        Ok(ExternalData {
                            uid: row.get(0)?,
                            name: row.get(1)?,
                            tool: row.get(2)?,
                            url_data: row.get(3)?,
                            url_view: row.get(4)?,
                            notes: row.get(5)?,
                        })
                    })?
                    .collect()
            })
            .await?)
    }

    /// Stores an external-data record, minting a uid when it carries none.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a record with no name, and a system
    /// error if the write fails.
    pub async fn put_external_data(
        &self,
        mission: &Mission,
        mut record: ExternalData,
        creator_uid: Option<&str>,
    ) -> Result<ExternalData, MartiError> {
        if record.name.trim().is_empty() {
            return Err(MartiError::InvalidRequest(
                "external data needs a name".to_string(),
            ));
        }

        let uid = record
            .uid
            .clone()
            .filter(|uid| !uid.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        record.uid = Some(uid.clone());

        let held = record.clone();
        let (mission_id, creator) = (mission.id, creator_uid.map(ToOwned::to_owned));

        self.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mission_external_data \
                       (mission_id, uid, name, tool, url_data, url_view, notes, creator_uid, \
                        created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) \
                     ON CONFLICT (mission_id, uid) DO UPDATE SET \
                       name = excluded.name, tool = excluded.tool, \
                       url_data = excluded.url_data, url_view = excluded.url_view, \
                       notes = excluded.notes, updated_at = excluded.updated_at",
                    rusqlite::params![
                        mission_id,
                        uid,
                        held.name,
                        held.tool,
                        held.url_data,
                        held.url_view,
                        held.notes,
                        creator,
                        Timestamp::now(),
                    ],
                )
            })
            .await?;

        Ok(record)
    }

    /// Removes an external-data record, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn delete_external_data(
        &self,
        mission: &Mission,
        uid: &str,
    ) -> Result<bool, MartiError> {
        let (mission_id, uid) = (mission.id, uid.to_string());

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM mission_external_data WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid],
                )? > 0)
            })
            .await?)
    }

    /// Records a data feed, which is listed and never acted on.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn put_feed(
        &self,
        mission: &Mission,
        uid: &str,
        body: serde_json::Value,
        creator_uid: Option<&str>,
    ) -> Result<(), MartiError> {
        let stored = serde_json::to_string(&body)?;
        let (mission_id, uid, creator) = (
            mission.id,
            uid.to_string(),
            creator_uid.map(ToOwned::to_owned),
        );

        self.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mission_feeds (mission_id, uid, body, creator_uid, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (mission_id, uid) DO UPDATE SET body = excluded.body",
                    rusqlite::params![mission_id, uid, stored, creator, Timestamp::now()],
                )
            })
            .await?;

        Ok(())
    }

    /// Every data feed registered against a mission, as stored.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn feeds(&self, mission: &Mission) -> Result<Vec<serde_json::Value>, MartiError> {
        let mission_id = mission.id;

        Ok(self
            .db()
            .read(move |c| {
                let mut statement =
                    c.prepare("SELECT body FROM mission_feeds WHERE mission_id = ?1 ORDER BY id")?;

                statement
                    .query_map(rusqlite::params![mission_id], |row| {
                        crate::db::row::json_col(row, 0)
                    })?
                    .collect()
            })
            .await?)
    }

    /// Removes a data feed, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn delete_feed(&self, mission: &Mission, uid: &str) -> Result<bool, MartiError> {
        let (mission_id, uid) = (mission.id, uid.to_string());

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM mission_feeds WHERE mission_id = ?1 AND uid = ?2",
                    rusqlite::params![mission_id, uid],
                )? > 0)
            })
            .await?)
    }
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

    #[tokio::test]
    async fn a_map_layer_comes_back_with_every_field_it_was_sent_with() {
        // The body is opaque: a client that adds a key of its own keeps it.
        let (_context, service, mission) = fixture().await;

        let stored = service
            .put_map_layer(
                &mission,
                serde_json::json!({
                    "name": "OSM",
                    "url": "https://tile.example.com/{z}/{x}/{y}.png",
                    "somethingWeHaveNeverHeardOf": 42,
                }),
                Some("ANDROID-1"),
            )
            .await
            .unwrap();

        assert!(!stored.uid.is_empty(), "a uid was minted");
        let read = service.map_layers(&mission).await.unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].body["somethingWeHaveNeverHeardOf"], 42);
        assert_eq!(read[0].body["uid"], stored.uid.as_str());
        assert_eq!(read[0].name.as_deref(), Some("OSM"));
    }

    #[tokio::test]
    async fn a_map_layer_sent_twice_under_one_uid_is_replaced() {
        let (_context, service, mission) = fixture().await;
        let first = service
            .put_map_layer(&mission, serde_json::json!({ "name": "OSM" }), None)
            .await
            .unwrap();

        service
            .put_map_layer(
                &mission,
                serde_json::json!({ "uid": first.uid, "name": "Satellite" }),
                None,
            )
            .await
            .unwrap();

        let read = service.map_layers(&mission).await.unwrap();

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name.as_deref(), Some("Satellite"));
        assert!(
            service
                .delete_map_layer(&mission, &first.uid)
                .await
                .unwrap()
        );
        assert!(service.map_layers(&mission).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_data_needs_a_name_and_round_trips() {
        let (_context, service, mission) = fixture().await;

        assert!(matches!(
            service
                .put_external_data(&mission, ExternalData::default(), None)
                .await,
            Err(MartiError::InvalidRequest(_)),
        ));

        let stored = service
            .put_external_data(
                &mission,
                ExternalData {
                    name: "Weather".to_string(),
                    tool: Some("wx".to_string()),
                    url_view: Some("https://example.com/wx".to_string()),
                    ..ExternalData::default()
                },
                Some("ANDROID-1"),
            )
            .await
            .unwrap();

        let read = service.external_data(&mission).await.unwrap();

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "Weather");
        assert_eq!(read[0].uid, stored.uid);
        assert!(
            service
                .delete_external_data(&mission, stored.uid.as_deref().unwrap())
                .await
                .unwrap()
        );
        assert!(service.external_data(&mission).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_feed_is_listed_and_nothing_more() {
        let (_context, service, mission) = fixture().await;

        service
            .put_feed(
                &mission,
                "feed-1",
                serde_json::json!({ "uid": "feed-1" }),
                None,
            )
            .await
            .unwrap();

        assert_eq!(service.feeds(&mission).await.unwrap().len(), 1);
        assert!(service.delete_feed(&mission, "feed-1").await.unwrap());
        assert!(!service.delete_feed(&mission, "feed-1").await.unwrap());
        assert!(service.feeds(&mission).await.unwrap().is_empty());
    }
}
