//! Filing things under a mission, and rendering what is filed.
//!
//! Two kinds of thing go into a mission: **map items**, named by the uid of the
//! CoT event that describes them, and **resources**, named by the content hash
//! of a file already in Enterprise Sync. Filing either one appends an
//! `ADD_CONTENT` change, which is what a subscriber's next `/changes` call — or
//! M4-02's `t-x-m-c` notice — reports.
//!
//! # Why a uid is cached rather than joined
//!
//! A filed map item is rendered with the type, callsign, icon, colour and
//! position of the event that described it *at the time it was filed*. Those
//! are copied onto the row here, from the CoT store's latest copy of the event,
//! so that listing a mission costs one query rather than one query per item —
//! and so that an item whose event has since aged out of the store still
//! renders.

use chrono::{DateTime, Utc};
use rustak_cot::Event;

use crate::db::repos::{MissionContentRow, MissionUidRow, NewChange};
use crate::marti::MartiError;

use super::dto::{Filed, LocationJson, MissionContentBody, UidDetailsJson};
use super::model::Mission;
use super::service::MissionService;

/// The change appended when something is filed under a mission.
pub const ADD_CONTENT: &str = "ADD_CONTENT";

/// The change appended when something is unfiled.
pub const REMOVE_CONTENT: &str = "REMOVE_CONTENT";
impl MissionService {
    /// Files everything a contents body named.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a body that names nothing, and
    /// [`MartiError::NotFound`] for a hash Enterprise Sync does not hold.
    pub async fn add_content(
        &self,
        mission: &Mission,
        body: &MissionContentBody,
        creator_uid: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<Vec<crate::db::repos::MissionChangeRow>, MartiError> {
        if body.is_empty() {
            return Err(MartiError::InvalidRequest(
                "at least one of hashes, uids or paths is required".to_string(),
            ));
        }

        let mut changes = Vec::new();

        for (layer, item) in body.flatten() {
            changes.push(match item {
                Filed::Hash(hash) => {
                    self.file_hash(mission, &hash, layer, creator_uid, at)
                        .await?
                }
                Filed::Uid(uid) => self.file_uid(mission, &uid, layer, creator_uid, at).await?,
            });
        }

        let recorded = self.db().mission_changes().record_all(changes).await?;

        // One `t-x-m-c` per change, to the connected subscribers minus the
        // author — a client that just filed something does not need telling.
        self.notify_content(mission, &recorded, creator_uid).await?;

        Ok(recorded)
    }

    /// Files one resource by content hash.
    async fn file_hash(
        &self,
        mission: &Mission,
        hash: &str,
        layer: Option<String>,
        creator_uid: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<NewChange, MartiError> {
        let resource = self
            .db()
            .resources()
            .by_hash(hash)
            .await?
            .ok_or_else(|| MartiError::NotFound(format!("Resource {hash}")))?;

        self.db()
            .mission_contents()
            .upsert_content(MissionContentRow {
                creator_uid: creator_uid.map(str::to_string),
                layer_uid: layer,
                ..MissionContentRow::new(mission.id, resource.id, at)
            })
            .await?;

        Ok(NewChange::new(mission.id, ADD_CONTENT, at)
            .by(creator_uid)
            .about_hash(resource.hash))
    }

    /// Files one map item by uid, caching what a listing renders it with.
    async fn file_uid(
        &self,
        mission: &Mission,
        uid: &str,
        layer: Option<String>,
        creator_uid: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<NewChange, MartiError> {
        let details = self.cached_details(uid).await?;

        self.db()
            .mission_contents()
            .upsert_uid(MissionUidRow {
                creator_uid: creator_uid.map(str::to_string),
                layer_uid: layer,
                details: details
                    .as_ref()
                    .and_then(|details| serde_json::to_value(details).ok()),
                ..MissionUidRow::new(mission.id, uid, at)
            })
            .await?;

        Ok(NewChange::new(mission.id, ADD_CONTENT, at)
            .by(creator_uid)
            .about_uid(uid)
            .with_detail(details.and_then(|details| serde_json::to_value(details).ok())))
    }

    /// The rendering fields of the latest event describing a uid.
    async fn cached_details(&self, uid: &str) -> Result<Option<UidDetailsJson>, MartiError> {
        let Some(xml) = crate::cot_store::latest_xml(self.db(), uid).await? else {
            return Ok(None);
        };

        Ok(rustak_cot::xml::parse_str(&xml)
            .ok()
            .map(|event| details_of(&event)))
    }

    /// Unfiles a resource, a map item, or both.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] when neither was named.
    pub async fn remove_content(
        &self,
        mission: &Mission,
        hash: Option<&str>,
        uid: Option<&str>,
        creator_uid: Option<&str>,
    ) -> Result<Vec<crate::db::repos::MissionChangeRow>, MartiError> {
        if hash.is_none() && uid.is_none() {
            return Err(MartiError::InvalidRequest(
                "hash or uid is required".to_string(),
            ));
        }

        let now = Utc::now();
        let mut changes = Vec::new();

        if let Some(hash) = hash {
            let removed = self
                .db()
                .mission_contents()
                .remove_content_by_hash(mission.id, hash.to_string())
                .await?;

            if !removed.is_empty() {
                changes.push(
                    NewChange::new(mission.id, REMOVE_CONTENT, now)
                        .by(creator_uid)
                        .about_hash(hash),
                );
            }
        }

        if let Some(uid) = uid
            && self
                .db()
                .mission_contents()
                .remove_uid(mission.id, uid.to_string())
                .await?
        {
            changes.push(
                NewChange::new(mission.id, REMOVE_CONTENT, now)
                    .by(creator_uid)
                    .about_uid(uid),
            );
        }

        let recorded = self.db().mission_changes().record_all(changes).await?;

        self.notify_content(mission, &recorded, creator_uid).await?;

        Ok(recorded)
    }

    /// Copies everything filed under one mission onto another.
    pub(super) async fn clone_contents(
        &self,
        from: &Mission,
        onto: &Mission,
        creator_uid: Option<&str>,
    ) -> Result<(), MartiError> {
        for item in self.db().mission_contents().uids(from.id).await? {
            self.db()
                .mission_contents()
                .upsert_uid(MissionUidRow {
                    mission_id: onto.id,
                    creator_uid: creator_uid.map(str::to_string).or(item.creator_uid.clone()),
                    ..item
                })
                .await?;
        }

        for content in self.db().mission_contents().contents(from.id).await? {
            self.db()
                .mission_contents()
                .upsert_content(MissionContentRow {
                    mission_id: onto.id,
                    creator_uid: creator_uid
                        .map(str::to_string)
                        .or(content.creator_uid.clone()),
                    ..content
                })
                .await?;
        }

        Ok(())
    }

    /// Unfiles everything a deleted mission held, for a deep delete.
    pub(super) async fn purge_contents(&self, mission: &Mission) -> Result<(), MartiError> {
        for item in self.db().mission_contents().uids(mission.id).await? {
            self.db()
                .mission_contents()
                .remove_uid(mission.id, item.uid)
                .await?;
        }

        for content in self.db().mission_contents().contents(mission.id).await? {
            if let Some(resource) = self.db().resources().by_id(content.resource_id).await? {
                self.db()
                    .mission_contents()
                    .remove_content_by_hash(mission.id, resource.hash.clone())
                    .await?;

                // Only when nothing else points at the blob; a hash shared with
                // another mission or an ordinary upload stays.
                crate::files::store::forget(&self.context, &resource.hash).await?;
            }
        }

        Ok(())
    }
}

/// The rendering fields of one event, as a filed item caches them.
pub fn details_of(event: &Event) -> UidDetailsJson {
    let contact = event.detail.find("contact");
    let icon = event.detail.find("usericon");
    let colour = event.detail.find("color");

    UidDetailsJson {
        kind: event.r#type.clone(),
        callsign: contact
            .and_then(|contact| contact.get("callsign"))
            .map(str::to_string),
        title: event
            .detail
            .find("archive")
            .and_then(|archive| archive.get("name"))
            .map(str::to_string),
        iconset_path: icon
            .and_then(|icon| icon.get("iconsetpath"))
            .map(str::to_string),
        color: colour
            .and_then(|colour| colour.get("argb"))
            .map(str::to_string),
        attachments: Vec::new(),
        name: contact
            .and_then(|contact| contact.get("callsign"))
            .map(str::to_string),
        category: None,
        location: Some(LocationJson {
            lat: event.point.lat,
            lon: event.point.lon,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_events_rendering_fields_are_read_off_its_detail() {
        let event = rustak_cot::xml::parse_str(concat!(
            "<event version='2.0' uid='ANDROID-1' type='a-f-G-U-C' time='2026-01-01T00:00:00Z' ",
            "start='2026-01-01T00:00:00Z' stale='2026-01-01T00:05:00Z'>",
            "<point lat='1.5' lon='-2.5' hae='0' ce='9999999' le='9999999'/>",
            "<detail><contact callsign='ALPHA'/><usericon iconsetpath='COT_MAPPING/a.png'/>",
            "<color argb='-1'/></detail></event>",
        ))
        .unwrap();

        let details = details_of(&event);

        assert_eq!(details.kind, "a-f-G-U-C");
        assert_eq!(details.callsign.as_deref(), Some("ALPHA"));
        assert_eq!(details.iconset_path.as_deref(), Some("COT_MAPPING/a.png"));
        assert_eq!(details.color.as_deref(), Some("-1"));
        assert_eq!(details.location.unwrap().lat, 1.5);
    }

    #[test]
    fn an_event_with_a_bare_detail_still_renders_its_type_and_position() {
        let event = rustak_cot::xml::parse_str(concat!(
            "<event version='2.0' uid='ANDROID-1' type='b-m-p' time='2026-01-01T00:00:00Z' ",
            "start='2026-01-01T00:00:00Z' stale='2026-01-01T00:05:00Z'>",
            "<point lat='0' lon='0' hae='0' ce='9999999' le='9999999'/></event>",
        ))
        .unwrap();

        let details = details_of(&event);

        assert_eq!(details.kind, "b-m-p");
        assert_eq!(details.callsign, None);
        assert!(details.location.is_some());
    }
}
