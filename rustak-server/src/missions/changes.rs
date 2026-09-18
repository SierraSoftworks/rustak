//! A mission's change log, squashed or in full, and its CoT view.
//!
//! # The squash
//!
//! `?squashed=true` — the default on `/changes` — asks for the *current-state
//! delta* rather than the history: what a client that has never seen this
//! mission would have to do to catch up. The rule, transcribed from TAK
//! Server's own six-way union and implemented here as a fold:
//!
//! 1. Keep the newest row per `(type, item, creator)`.
//! 2. Keep an `ADD_CONTENT` only if the item is still filed.
//! 3. Keep a `REMOVE_CONTENT` only if it is not.
//! 4. Always keep the rows that are not about an item at all — the mission's
//!    own creation and deletion, and data-feed rows.
//!
//! [`squash`] is one reverse pass over the window; [`crate::missions`]'s tests
//! hold it against a naive model that scans the whole list per key, because the
//! two agreeing is the only evidence the fold is right.
//!
//! `?squashed=false` — and the `?changes=true` form of `GET /missions/{name}`,
//! which is genuinely the other default — returns every row.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rustak_cot::Event;

use crate::db::repos::MissionChangeRow;
use crate::marti::{MartiError, time};

use super::dto::{MissionChangeJson, UidDetailsJson};
use super::model::Mission;
use super::service::MissionService;

/// The XML declaration a mission's CoT view opens with.
pub const EVENTS_PROLOGUE: &str = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?><events>";

/// What is filed under a mission right now.
///
/// Passed to [`squash`] rather than read inside it, so that the fold is a pure
/// function a property test can drive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presence {
    /// The map items still filed.
    pub uids: HashSet<String>,
    /// The content hashes still filed.
    pub hashes: HashSet<String>,
}

impl Presence {
    /// Whether the item a change is about is still filed.
    ///
    /// A change about nothing in particular — a mission create or delete, a
    /// data feed — is always "present", which is what keeps it in the squash.
    fn holds(&self, change: &MissionChangeRow) -> bool {
        match (&change.content_uid, &change.content_hash) {
            (Some(uid), _) => self.uids.contains(uid),
            (None, Some(hash)) => self.hashes.contains(hash),
            (None, None) => true,
        }
    }
}

/// Whether a change is about a specific filed item.
fn is_about_item(change: &MissionChangeRow) -> bool {
    change.content_uid.is_some() || change.content_hash.is_some()
}

/// The key a squash keeps one row per.
fn key(change: &MissionChangeRow) -> (String, String, String) {
    (
        change.kind.clone(),
        change
            .content_uid
            .clone()
            .or_else(|| change.content_hash.clone())
            .unwrap_or_default(),
        change.creator_uid.clone().unwrap_or_default(),
    )
}

/// Reduces a window of changes to the current-state delta.
///
/// `changes` arrives newest first, as the repository returns it, and the answer
/// is in the same order.
pub fn squash(changes: Vec<MissionChangeRow>, present: &Presence) -> Vec<MissionChangeRow> {
    let mut seen: HashSet<(String, String, String)> = HashSet::new();

    changes
        .into_iter()
        .filter(|change| seen.insert(key(change)))
        .filter(|change| {
            if !is_about_item(change) {
                return true;
            }

            match change.kind.as_str() {
                super::contents::ADD_CONTENT => present.holds(change),
                super::contents::REMOVE_CONTENT => !present.holds(change),
                _ => true,
            }
        })
        .collect()
}

impl MissionService {
    /// A mission's changes inside a window, squashed or in full.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn changes(
        &self,
        mission: &Mission,
        window: time::TimeWindow,
        squashed: bool,
    ) -> Result<Vec<MissionChangeJson>, MartiError> {
        let rows = self
            .db()
            .mission_changes()
            .window(mission.id, window.start, window.end)
            .await?;

        let rows = match squashed {
            true => squash(rows, &self.presence(mission).await?),
            false => rows,
        };

        self.render_changes(mission, rows).await
    }

    /// Every change recorded since an instant, in full.
    ///
    /// The hook M4-02's notice fan-out reads: it needs the rows a write
    /// produced, not the delta a catching-up client would want.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn changes_since(
        &self,
        mission: &Mission,
        since: DateTime<Utc>,
    ) -> Result<Vec<MissionChangeJson>, MartiError> {
        let rows = self
            .db()
            .mission_changes()
            .window(mission.id, since, Utc::now())
            .await?;

        self.render_changes(mission, rows).await
    }

    /// Renders change rows as the wire shape.
    ///
    /// Public because M4-02 renders the rows [`MissionService::add_content`]
    /// and its siblings hand back, to put inside a `t-x-m-c` notice.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a resource lookup fails.
    pub async fn render_changes(
        &self,
        mission: &Mission,
        rows: Vec<MissionChangeRow>,
    ) -> Result<Vec<MissionChangeJson>, MartiError> {
        let mut rendered = Vec::with_capacity(rows.len());

        for row in rows {
            let content_resource = match &row.content_hash {
                Some(hash) => self
                    .db()
                    .resources()
                    .by_hash(hash)
                    .await?
                    .as_ref()
                    .map(crate::files::resource_json),
                None => None,
            };

            rendered.push(MissionChangeJson {
                kind: row.kind,
                timestamp: time::cot_date(row.timestamp),
                server_time: time::cot_date(row.server_time),
                mission_name: mission.name.clone(),
                mission_guid: mission.guid.to_string(),
                is_federated_change: row.is_federated,
                content_uid: row.content_uid,
                creator_uid: row.creator_uid,
                details: row
                    .detail
                    .and_then(|detail| serde_json::from_value::<UidDetailsJson>(detail).ok()),
                content_resource,
            });
        }

        Ok(rendered)
    }

    /// What is filed under a mission right now.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn presence(&self, mission: &Mission) -> Result<Presence, MartiError> {
        let uids = self
            .db()
            .mission_contents()
            .uids(mission.id)
            .await?
            .into_iter()
            .map(|row| row.uid)
            .collect();

        let mut hashes = HashSet::new();

        for content in self.db().mission_contents().contents(mission.id).await? {
            if let Some(resource) = self.db().resources().by_id(content.resource_id).await? {
                hashes.insert(resource.hash);
            }
        }

        Ok(Presence { uids, hashes })
    }

    /// The latest CoT for every map item filed under a mission.
    ///
    /// Answers `<events></events>` for a mission holding nothing rather than a
    /// `404`: CloudTAK renders the result of this call directly and a missing
    /// document is a broken layer rather than an empty one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn cot_events_xml(
        &self,
        mission: &Mission,
        path: Option<&str>,
    ) -> Result<String, MartiError> {
        let mut document = String::from(EVENTS_PROLOGUE);

        for item in self.db().mission_contents().uids(mission.id).await? {
            if path.is_some_and(|path| item.layer_uid.as_deref() != Some(path)) {
                continue;
            }

            if let Some(xml) = crate::cot_store::latest_xml(self.db(), &item.uid).await? {
                document.push_str(&without_marti(&xml));
                document.push('\n');
            }
        }

        document.push_str("</events>");

        Ok(document)
    }
}

/// One event's XML with its `<marti>` routing block removed.
///
/// `<marti>` says who the sender addressed the event to, which is a fact about
/// one delivery rather than about the object — replaying it to a client reading
/// a mission would tell them about recipients they have nothing to do with.
/// An event we cannot re-parse is passed through unchanged rather than dropped.
pub fn without_marti(xml: &str) -> String {
    let Ok(mut event) = rustak_cot::xml::parse_str(xml) else {
        return xml.to_string();
    };

    event.detail.remove_all("marti");

    String::from_utf8_lossy(&rustak_cot::xml::write(&event)).into_owned()
}

/// The rendering fields of an event, for callers outside this module.
pub fn details_of(event: &Event) -> UidDetailsJson {
    super::contents::details_of(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(
        id: i64,
        kind: &str,
        uid: Option<&str>,
        creator: &str,
        seconds: i64,
    ) -> MissionChangeRow {
        let at = DateTime::UNIX_EPOCH + chrono::Duration::seconds(seconds);

        MissionChangeRow {
            id,
            mission_id: 1,
            kind: kind.to_string(),
            timestamp: at,
            server_time: at,
            creator_uid: Some(creator.to_string()),
            content_uid: uid.map(str::to_string),
            content_hash: None,
            log_entry_id: None,
            map_layer_uid: None,
            feed_uid: None,
            is_federated: false,
            detail: None,
        }
    }

    /// Newest first, as the repository returns a window.
    fn newest_first(mut rows: Vec<MissionChangeRow>) -> Vec<MissionChangeRow> {
        rows.sort_by(|a, b| b.server_time.cmp(&a.server_time).then(b.id.cmp(&a.id)));
        rows
    }

    fn present(uids: &[&str]) -> Presence {
        Presence {
            uids: uids.iter().map(|uid| (*uid).to_string()).collect(),
            hashes: HashSet::new(),
        }
    }

    #[test]
    fn an_add_then_remove_then_add_squashes_to_one_add() {
        let history = newest_first(vec![
            change(1, super::super::contents::ADD_CONTENT, Some("a"), "c1", 10),
            change(
                2,
                super::super::contents::REMOVE_CONTENT,
                Some("a"),
                "c1",
                20,
            ),
            change(3, super::super::contents::ADD_CONTENT, Some("a"), "c1", 30),
        ]);

        let squashed = squash(history, &present(&["a"]));

        assert_eq!(squashed.len(), 1);
        assert_eq!(squashed[0].id, 3);
    }

    #[test]
    fn a_removed_item_keeps_its_remove_and_drops_its_add() {
        let history = newest_first(vec![
            change(1, super::super::contents::ADD_CONTENT, Some("a"), "c1", 10),
            change(
                2,
                super::super::contents::REMOVE_CONTENT,
                Some("a"),
                "c1",
                20,
            ),
        ]);

        let squashed = squash(history, &present(&[]));

        assert_eq!(squashed.len(), 1);
        assert_eq!(squashed[0].kind, super::super::contents::REMOVE_CONTENT);
    }

    #[test]
    fn the_missions_own_rows_always_survive() {
        let history = newest_first(vec![
            change(1, "CREATE_MISSION", None, "c1", 5),
            change(2, super::super::contents::ADD_CONTENT, Some("a"), "c1", 10),
        ]);

        let squashed = squash(history, &present(&[]));

        assert_eq!(squashed.len(), 1);
        assert_eq!(squashed[0].kind, "CREATE_MISSION");
    }

    #[test]
    fn two_creators_filing_the_same_item_are_two_rows() {
        let history = newest_first(vec![
            change(1, super::super::contents::ADD_CONTENT, Some("a"), "c1", 10),
            change(2, super::super::contents::ADD_CONTENT, Some("a"), "c2", 20),
        ]);

        let squashed = squash(history, &present(&["a"]));

        assert_eq!(squashed.len(), 2);
    }

    #[test]
    fn stripping_marti_leaves_the_rest_of_the_event_alone() {
        let stripped = without_marti(concat!(
            "<event version='2.0' uid='ANDROID-1' type='a-f-G' time='2026-01-01T00:00:00Z' ",
            "start='2026-01-01T00:00:00Z' stale='2026-01-01T00:05:00Z'>",
            "<point lat='1' lon='2' hae='0' ce='9999999' le='9999999'/>",
            "<detail><contact callsign='ALPHA'/><marti><dest mission='Alpha'/></marti></detail>",
            "</event>",
        ));

        assert!(!stripped.contains("marti"), "{stripped}");
        assert!(stripped.contains("ALPHA"), "{stripped}");
    }

    #[test]
    fn an_event_we_cannot_parse_is_passed_through() {
        assert_eq!(without_marti("not xml"), "not xml");
    }
}
