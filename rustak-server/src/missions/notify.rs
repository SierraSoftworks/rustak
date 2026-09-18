//! Turning what just happened to a mission into the notice its watchers get.
//!
//! The service writes, then calls in here; this module decides who should hear
//! about it and hands [`crate::stream::mission_notify`] a value to render. The
//! split is deliberate: the stream module knows the exact `t-x-m-*` template
//! and nothing about missions, and this module knows the mission rules and
//! nothing about connections.
//!
//! # A notice is never the reason a write fails
//!
//! Every function here returns how many connections it reached, and reaching
//! nobody is the ordinary case — an installation with the stream listener
//! turned off, a mission whose subscribers are all offline, a change made over
//! REST by somebody with no stream connection at all. The change is already
//! durable by the time we get here, and the subscriber reads it back from
//! `/changes` on its next poll, so a failure to notify is logged and swallowed
//! rather than propagated into the caller's `Result`.

use chrono::{DateTime, Utc};

use crate::db::repos::{MissionChangeRow, ResourceRow};
use crate::marti::MartiError;
use crate::prelude::*;
use crate::stream::mission_notify::{self, ChangeKind, MissionNotice, NoticeMission, Recipients};
use crate::stream::mission_payload::{
    MissionChangeXml, MissionLayerXml, MissionRoleXml, ResourceXml, UidDetailsXml,
};

use super::dto::UidDetailsJson;
use super::model::Mission;
use super::roles::Role;
use super::service::MissionService;

/// The admin API's spelling of a mission role, for the notice renderer.
///
/// The stream module renders `<role>` from `MissionRoleKind` so that it does
/// not have to reach into the mission service for a permission list; this is
/// the one place the two vocabularies meet.
impl From<Role> for rustak_api::MissionRoleKind {
    fn from(role: Role) -> Self {
        match role {
            Role::Owner => Self::Owner,
            Role::Subscriber => Self::Subscriber,
            Role::ReadonlySubscriber => Self::ReadonlySubscriber,
        }
    }
}

/// The mission, as a notice names it.
pub fn notice_mission(mission: &Mission) -> NoticeMission {
    NoticeMission {
        name: mission.name.clone(),
        guid: mission.guid.to_string(),
        tool: mission.tool.clone(),
        groups: mission
            .effective_groups()
            .iter()
            .filter_map(|name| GroupName::parse(name).ok())
            .collect(),
    }
}

impl MissionService {
    /// Pushes a notice at whoever it is for.
    ///
    /// Answers how many connections it reached, which is `0` on an installation
    /// with no stream listener — that is a configuration, not a fault.
    pub fn notify(&self, notice: &MissionNotice) -> usize {
        // The server-event feed hears about it whether or not anybody is
        // connected: a sidecar watching `/api/v1/events` is not a stream client.
        self.context.events().mission_changed(notice);

        if !self.context.has_live() {
            return 0;
        }

        match self.context.live() {
            Ok(live) => mission_notify::send(live.notifier().as_ref(), notice),
            Err(err) => {
                debug!(error = %err, "A mission notice had nowhere to go.");
                0
            }
        }
    }

    /// `t-x-m-n`: a mission was created, announced to the channels it belongs
    /// to.
    pub fn notify_created(&self, mission: &Mission, author_uid: Option<&str>) -> usize {
        self.notify(&MissionNotice::Created {
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
        })
    }

    /// `t-x-m-d`: a mission was deleted, announced the same way.
    pub fn notify_deleted(&self, mission: &Mission, author_uid: Option<&str>) -> usize {
        self.notify(&MissionNotice::Deleted {
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
        })
    }

    /// `t-x-m-i`: somebody was invited, told only to the uids resolved for them.
    pub fn notify_invited(
        &self,
        mission: &Mission,
        author_uid: Option<&str>,
        token: &str,
        role: Role,
        uids: Vec<String>,
    ) -> usize {
        if uids.is_empty() {
            return 0;
        }

        self.notify(&MissionNotice::Invite {
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
            token: token.to_string(),
            role: MissionRoleXml(role.into()),
            uids,
        })
    }

    /// `t-x-m-r`: one subscription's role changed, told to that uid alone.
    pub fn notify_role_changed(
        &self,
        mission: &Mission,
        author_uid: Option<&str>,
        role: Role,
        client_uid: &str,
    ) -> usize {
        self.notify(&MissionNotice::RoleChange {
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
            role: MissionRoleXml(role.into()),
            uid: client_uid.to_string(),
        })
    }

    /// The device uids a role change has to be told about.
    ///
    /// A change addressed by account reaches every device that account has
    /// subscribed with, which is more than one whenever somebody carries a
    /// phone and a tablet.
    pub(super) async fn role_change_recipients(
        &self,
        mission: &Mission,
        client_uid: Option<&str>,
        username: Option<&str>,
    ) -> Result<Vec<String>, MartiError> {
        if let Some(uid) = client_uid {
            return Ok(vec![uid.to_string()]);
        }

        let Some(username) = username else {
            return Ok(Vec::new());
        };

        Ok(self
            .subscriptions(mission)
            .await?
            .into_iter()
            .filter(|held| {
                held.username
                    .as_deref()
                    .is_some_and(|held| held.eq_ignore_ascii_case(username))
            })
            .map(|held| held.client_uid)
            .collect())
    }

    /// A `t-x-m-c…` broadcast to everyone who may read the mission.
    ///
    /// The metadata and mission-keyword notices go this way rather than to the
    /// subscribers, because a change to the mission itself is visible to people
    /// who have not subscribed to it yet.
    pub fn notify_broadcast(
        &self,
        mission: &Mission,
        kind: ChangeKind,
        author_uid: Option<&str>,
    ) -> usize {
        let named = notice_mission(mission);
        let groups = named.groups.clone();

        self.notify(&MissionNotice::Change {
            kind,
            mission: named,
            author_uid: author_uid.map(ToOwned::to_owned),
            changes: Vec::new(),
            layer: None,
            recipients: Recipients::BroadcastGroups(groups),
        })
    }

    /// A `t-x-m-c…` carrying no payload, to the connected subscribers.
    ///
    /// What a uid-keyword, resource-keyword, log or external-data change sends:
    /// the client is being told *that* something changed and re-reads the
    /// mission, rather than being handed the change itself.
    ///
    /// # Errors
    ///
    /// Whatever reading the subscription list reported.
    pub async fn notify_subscribers(
        &self,
        mission: &Mission,
        kind: ChangeKind,
        author_uid: Option<&str>,
    ) -> Result<usize, MartiError> {
        let recipients = self.subscribers_for(mission, author_uid).await?;

        if recipients.is_empty() {
            return Ok(0);
        }

        Ok(self.notify(&MissionNotice::Change {
            kind,
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
            changes: Vec::new(),
            layer: None,
            recipients: Recipients::Subscribers(recipients),
        }))
    }

    /// `t-x-m-c-h`: the layer tree changed, with the layer that changed.
    ///
    /// # Errors
    ///
    /// Whatever reading the subscription list reported.
    pub async fn notify_layer(
        &self,
        mission: &Mission,
        layer: MissionLayerXml,
        author_uid: Option<&str>,
    ) -> Result<usize, MartiError> {
        let recipients = self.subscribers_for(mission, author_uid).await?;

        if recipients.is_empty() {
            return Ok(0);
        }

        Ok(self.notify(&MissionNotice::Change {
            kind: ChangeKind::Layer,
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
            changes: Vec::new(),
            layer: Some(layer),
            recipients: Recipients::Subscribers(recipients),
        }))
    }

    /// One `t-x-m-c` per change, to the connected subscribers minus the author.
    ///
    /// The rows are rendered one per event because CloudTAK only reads
    /// `missionChanges[0]`, and each one is enriched with the stored resource
    /// when it names a hash — the client's add/remove list is built from
    /// `contentResource.name`, so a notice without it shows nothing.
    ///
    /// # Errors
    ///
    /// Whatever reading the subscriptions or the resources reported.
    pub async fn notify_content(
        &self,
        mission: &Mission,
        rows: &[MissionChangeRow],
        author_uid: Option<&str>,
    ) -> Result<usize, MartiError> {
        if rows.is_empty() {
            return Ok(0);
        }

        let recipients = self.subscribers_for(mission, author_uid).await?;

        if recipients.is_empty() {
            return Ok(0);
        }

        let mut changes = Vec::with_capacity(rows.len());

        for row in rows {
            changes.push(self.change_xml(row).await?);
        }

        Ok(self.notify(&MissionNotice::Change {
            kind: ChangeKind::Content,
            mission: notice_mission(mission),
            author_uid: author_uid.map(ToOwned::to_owned),
            changes,
            layer: None,
            recipients: Recipients::Subscribers(recipients),
        }))
    }

    /// One stored change row, as the notice carries it.
    async fn change_xml(&self, row: &MissionChangeRow) -> Result<MissionChangeXml, MartiError> {
        let mut change = MissionChangeXml::new(row.kind.clone(), row.timestamp);
        change.content_uid = row.content_uid.clone();
        change.creator_uid = row.creator_uid.clone();
        change.details = row
            .detail
            .clone()
            .and_then(|detail| serde_json::from_value::<UidDetailsJson>(detail).ok())
            .map(details_xml);

        if let Some(hash) = &row.content_hash {
            change.resource = self
                .db()
                .resources()
                .by_hash(hash)
                .await?
                .map(|resource| resource_xml(&resource));
        }

        Ok(change)
    }
}

/// The cached item description, as `<details …/>` carries it.
fn details_xml(details: UidDetailsJson) -> UidDetailsXml {
    UidDetailsXml {
        kind: Some(details.kind).filter(|kind| !kind.is_empty()),
        callsign: details.callsign,
        title: details.title,
        iconset_path: details.iconset_path,
        color: details.color,
        location: details.location.map(|at| (at.lat, at.lon)),
    }
}

/// A stored resource, as `<contentResource>` carries it.
fn resource_xml(resource: &ResourceRow) -> ResourceXml {
    ResourceXml {
        creator_uid: resource.creator_uid.clone().unwrap_or_default(),
        // TAK spells "never" as `-1` here rather than omitting the field.
        expiration: resource.expiration.unwrap_or(-1),
        filename: resource
            .filename
            .clone()
            .unwrap_or_else(|| resource.name.clone()),
        hash: resource.hash.clone(),
        keywords: resource.keywords.clone(),
        mime_type: resource.mime_type.clone(),
        name: resource.name.clone(),
        size: u64::try_from(resource.size).unwrap_or_default(),
        submission_time: resource.submission_time,
        submitter: resource.submitter.clone().unwrap_or_default(),
        tool: resource.tool.clone(),
        uid: resource.uid.clone(),
    }
}

/// A synthetic change row, for a notice about something with no stored row.
///
/// The log and layer endpoints announce themselves without appending to the
/// change log, and a client that reads `missionChanges[0].type` still wants a
/// type to read.
pub fn synthetic(kind: &str, at: DateTime<Utc>, creator_uid: Option<&str>) -> MissionChangeXml {
    let mut change = MissionChangeXml::new(kind, at);
    change.creator_uid = creator_uid.map(ToOwned::to_owned);
    change
}

/// The instant a synthetic change is dated, which is always now.
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use rustak_api::MissionRoleKind;

    use super::*;

    fn mission() -> Mission {
        Mission {
            id: 1,
            guid: uuid::Uuid::nil(),
            name: "Operation Kettle".to_string(),
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
            create_time: DateTime::UNIX_EPOCH,
            last_edited: None,
            default_role: Role::Subscriber,
            invite_only: false,
            password_hash: None,
            expiration: None,
            groups: vec!["Blue".to_string()],
            parent_id: None,
            deleted_at: None,
        }
    }

    #[test]
    fn a_notice_names_the_mission_by_name_guid_tool_and_channels() {
        let named = notice_mission(&mission());

        assert_eq!(named.name, "Operation Kettle");
        assert_eq!(named.tool, "public");
        assert_eq!(named.groups, vec![GroupName::parse("Blue").unwrap()]);
    }

    #[test]
    fn a_mission_in_no_channel_is_announced_on_the_default_one() {
        // `effective_groups` substitutes `__ANON__`, which the broadcast path
        // reads as "everybody" — a public mission nobody scoped is public.
        let mission = Mission {
            groups: Vec::new(),
            ..mission()
        };

        assert_eq!(notice_mission(&mission).groups, vec![GroupName::anon()]);
    }

    #[test]
    fn a_role_renders_the_permissions_the_wire_expects() {
        assert_eq!(
            MissionRoleXml(MissionRoleKind::from(Role::Owner))
                .permissions()
                .len(),
            8
        );
        assert_eq!(
            MissionRoleXml(MissionRoleKind::from(Role::ReadonlySubscriber)).permissions(),
            &["MISSION_READ"],
        );
    }

    #[test]
    fn an_empty_item_type_is_omitted_rather_than_emitted_blank() {
        let details = details_xml(UidDetailsJson {
            kind: String::new(),
            callsign: Some("ALPHA".to_string()),
            ..UidDetailsJson::default()
        });

        assert_eq!(details.kind, None);
        assert_eq!(details.callsign.as_deref(), Some("ALPHA"));
    }

    #[test]
    fn a_synthetic_change_carries_only_what_it_was_given() {
        let change = synthetic("ADD_CONTENT", DateTime::UNIX_EPOCH, Some("ANDROID-1"));

        assert_eq!(change.kind, "ADD_CONTENT");
        assert_eq!(change.creator_uid.as_deref(), Some("ANDROID-1"));
        assert_eq!(change.content_uid, None);
        assert!(change.resource.is_none());
        assert!(now() > DateTime::UNIX_EPOCH);
    }
}
