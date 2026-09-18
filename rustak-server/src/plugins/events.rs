//! The server-event bus behind `GET /api/v1/events`.
//!
//! One `tokio::sync::broadcast` channel, one bounded ring of what it has
//! recently carried, and a handful of methods the rest of the server calls when
//! something worth announcing happens. [`ServerEvents`] lives on the
//! [`crate::services::AppContext`], so a hook is one line at the
//! place that already knows what happened rather than a plumbed handle.
//!
//! # Why broadcast, and what a slow consumer costs
//!
//! Every subscriber gets every event, and a subscriber that falls
//! [`RING`] events behind is told it lagged rather than being waited for.
//! That is the trade the feed is built on: the server never blocks on a plugin,
//! and a plugin that could not keep up learns that it missed something and
//! re-reads whatever it cares about. A channel that queued instead would let one
//! stalled sidecar hold a stream connection's unregister path open.
//!
//! # Resuming
//!
//! Each event carries a `u64` id that increases by one, and the ring holds the
//! last [`RING`] of them, so a consumer that reconnects with `Last-Event-ID`
//! gets what it missed — as long as it missed fewer than a ring's worth. The
//! ids restart at 1 when the process does, which a consumer notices as an id
//! lower than the one it last saw; nothing is persisted, because an event that
//! was worth announcing an hour ago is not worth replaying after a restart.
//!
//! # Nothing secret crosses this bus
//!
//! The payloads are `rustak_api::event`'s, which carry names, uids and sizes.
//! No tokens, no certificate material, no file contents, no peer addresses —
//! see that module. A consumer of this feed is authenticated but is not
//! necessarily an administrator, so "what a service may see" is the bar.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use parking_lot::Mutex;
use rustak_api::event::{
    ChannelEvent, ClientEvent, MissionEvent, PackageEvent, ServerEvent, ServerEventPayload,
    ServiceEvent,
};
use rustak_api::{ServiceName, ServiceState};
use tokio::sync::broadcast;

use crate::db::repos::ResourceRow;
use crate::prelude::*;
use crate::stream::live::{ConnectionChange, ConnectionWatcher};
use crate::stream::mission_notify::MissionNotice;

/// How many recent events are kept for `Last-Event-ID` to resume from.
///
/// Also the broadcast channel's depth, so that "too far behind to resume" and
/// "too far behind to keep receiving" are the same distance: a consumer that
/// survives a lag can always resume from the ring.
pub const RING: usize = 256;

/// The `action` [`files::upload::audit`](crate::files::upload::audit) uses for
/// a file that has just arrived, which is the only one the feed reports.
const UPLOADED: &str = "uploaded";

/// The bus every server event is published on.
///
/// Cheap to clone — one [`Arc`] — and safe to hold: publishing into a bus with
/// no subscribers is a counter increment and a push onto the ring.
#[derive(Clone)]
pub struct ServerEvents {
    inner: Arc<Inner>,
}

struct Inner {
    sender: broadcast::Sender<ServerEvent>,
    next_id: AtomicU64,
    recent: Mutex<VecDeque<ServerEvent>>,
}

impl Default for ServerEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerEvents {
    /// A bus with nothing on it and nobody listening.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(RING);

        Self {
            inner: Arc::new(Inner {
                sender,
                next_id: AtomicU64::new(0),
                recent: Mutex::new(VecDeque::with_capacity(RING)),
            }),
        }
    }

    /// Starts receiving events published from now on.
    ///
    /// The returned receiver reports [`RecvError::Lagged`] rather than dropping
    /// silently when its holder falls behind; `web::api::events` turns that into
    /// a replay from the ring.
    ///
    /// [`RecvError::Lagged`]: broadcast::error::RecvError::Lagged
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.inner.sender.subscribe()
    }

    /// Everything still held that happened after `id`, oldest first.
    ///
    /// This is `Last-Event-ID` resume. An `id` older than the ring gives
    /// everything the ring holds, which is the honest answer to "I have been
    /// away too long": the consumer sees a gap in the ids and can re-read
    /// whatever it keeps state about.
    pub fn since(&self, id: u64) -> Vec<ServerEvent> {
        self.inner
            .recent
            .lock()
            .iter()
            .filter(|event| event.id > id)
            .cloned()
            .collect()
    }

    /// The id the next event will carry, for a consumer that only wants what
    /// happens from now on.
    pub fn latest_id(&self) -> u64 {
        self.inner.next_id.load(Ordering::Relaxed)
    }

    /// Publishes one event, and answers what it was given.
    ///
    /// Never fails: a bus with no subscribers is the ordinary case, and an event
    /// nobody was listening for is still worth keeping for the next consumer to
    /// resume over.
    pub fn publish(&self, payload: ServerEventPayload) -> ServerEvent {
        let event = ServerEvent {
            id: self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1,
            at: Utc::now(),
            payload,
        };

        {
            let mut recent = self.inner.recent.lock();

            if recent.len() == RING {
                recent.pop_front();
            }

            recent.push_back(event.clone());
        }

        // `Err` means nobody is subscribed, which is not a failure.
        let _ = self.inner.sender.send(event.clone());

        event
    }

    /// Reports every connection that joins or leaves the CoT stream.
    ///
    /// Called once, by
    /// [`AppContext::install_live`](crate::services::AppContext::install_live),
    /// at the one moment both this bus and the stream registry exist. The
    /// watcher runs on the connecting task, so it does the least possible work:
    /// build a payload, push it, return.
    pub fn watch(&self, live: &crate::stream::LiveState) {
        let events = self.clone();

        live.watch_connections(ConnectionWatcher::new(move |change| {
            let connection = change.connection();
            let client = ClientEvent {
                username: connection.username.to_string(),
                uid: connection.client_uid.clone(),
                callsign: connection.callsign.clone(),
            };

            events.publish(match change {
                ConnectionChange::Joined(_) => ServerEventPayload::ClientConnected(client),
                ConnectionChange::Left(_) => ServerEventPayload::ClientDisconnected(client),
            });
        }));
    }

    /// `mission.changed`: something happened to a mission.
    ///
    /// Takes the stream notice rather than its parts, so that the hook in
    /// `missions::notify` is one line and the mapping from a `t-x-m-*` notice to
    /// a feed event lives beside the rest of the feed. `change` on the wire is
    /// that notice's CoT type, which is what tells a consumer whether it was a
    /// creation, a content change, an invitation or a deletion — the same
    /// vocabulary a client watching the stream would see.
    pub fn mission_changed(&self, notice: &MissionNotice) {
        let mission = notice.mission();

        self.publish(ServerEventPayload::MissionChanged(MissionEvent {
            name: mission.name.clone(),
            guid: Some(mission.guid.clone()),
            change: change_type(notice).to_string(),
            author_uid: notice.author_uid().map(ToOwned::to_owned),
        }));
    }

    /// `channel.changed`: an account's channels were re-evaluated.
    pub fn channel_changed(&self, username: &Username) {
        self.publish(ServerEventPayload::ChannelChanged(ChannelEvent {
            username: username.to_string(),
        }));
    }

    /// `package.uploaded`, for the one file audit action that is an arrival.
    ///
    /// Given the action rather than called only for uploads, so that the hook in
    /// [`files::upload::audit`](crate::files::upload::audit) is one
    /// unconditional line and the decision about which actions are announced
    /// stays here.
    pub fn package(&self, action: &str, resource: &ResourceRow) {
        if action != UPLOADED {
            return;
        }

        self.publish(ServerEventPayload::PackageUploaded(PackageEvent {
            uid: resource.uid.clone(),
            name: resource.name.clone(),
            hash: resource.hash.clone(),
            size: resource.size,
            mission_package: resource.is_mission_package,
            submitter: resource.submitter.clone(),
        }));
    }

    /// `service.status`: a registered service said how it is doing.
    pub fn service_status(&self, name: &ServiceName, state: ServiceState, message: Option<&str>) {
        self.publish(ServerEventPayload::ServiceStatus(ServiceEvent {
            name: name.clone(),
            state,
            message: message.map(ToOwned::to_owned),
        }));
    }
}

/// The CoT type the stream would carry for this notice.
///
/// Mirrors what `stream::mission_notify` renders, rather than reaching into it,
/// because the feed's `change` field is a *name for what happened* and would go
/// on meaning that if the wire template ever moved.
fn change_type(notice: &MissionNotice) -> &'static str {
    use rustak_cot::types::cot_type;

    match notice {
        MissionNotice::Change { kind, .. } => kind.cot_type(),
        MissionNotice::Created { .. } => cot_type::MISSION_CREATE,
        MissionNotice::Deleted { .. } => cot_type::MISSION_DELETE,
        MissionNotice::Invite { .. } => cot_type::MISSION_INVITE,
        MissionNotice::RoleChange { .. } => cot_type::MISSION_ROLE_CHANGE,
    }
}

impl std::fmt::Debug for ServerEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerEvents")
            .field("published", &self.latest_id())
            .field("subscribers", &self.inner.sender.receiver_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(events: &ServerEvents, username: &str) -> ServerEvent {
        events.publish(ServerEventPayload::ChannelChanged(ChannelEvent {
            username: username.to_string(),
        }))
    }

    #[test]
    fn ids_start_at_one_and_increase_by_one() {
        // `Last-Event-ID` resume is arithmetic on these, so a gap or a repeat
        // would silently lose or duplicate an event for every consumer.
        let events = ServerEvents::new();

        assert_eq!(events.latest_id(), 0);
        assert_eq!(channel(&events, "ada").id, 1);
        assert_eq!(channel(&events, "grace").id, 2);
        assert_eq!(events.latest_id(), 2);
    }

    #[tokio::test]
    async fn a_subscriber_receives_what_is_published_after_it_subscribed() {
        let events = ServerEvents::new();
        channel(&events, "before");

        let mut receiver = events.subscribe();
        channel(&events, "after");

        let received = receiver.recv().await.unwrap();

        assert_eq!(received.name(), "channel.changed");
        assert_eq!(received.id, 2, "the id is the bus's, not the receiver's");
    }

    #[test]
    fn publishing_with_nobody_listening_is_not_a_failure() {
        // The ordinary case: an installation with no sidecars still records
        // channel changes, and they are still there for the first one to
        // connect.
        let events = ServerEvents::new();

        channel(&events, "ada");

        assert_eq!(events.since(0).len(), 1);
    }

    #[test]
    fn resuming_gives_only_what_came_after_the_id_the_consumer_saw() {
        let events = ServerEvents::new();
        for name in ["a", "b", "c"] {
            channel(&events, name);
        }

        let resumed = events.since(1);

        assert_eq!(resumed.len(), 2);
        assert_eq!(resumed[0].id, 2);
        assert_eq!(resumed[1].id, 3);
        assert!(events.since(3).is_empty());
    }

    #[test]
    fn the_ring_is_bounded_and_a_consumer_that_was_away_too_long_sees_the_gap() {
        let events = ServerEvents::new();
        for index in 0..RING + 10 {
            channel(&events, &format!("user{index}"));
        }

        let everything = events.since(0);

        assert_eq!(everything.len(), RING);
        assert_eq!(
            everything[0].id, 11,
            "the oldest ten were dropped, and the id says so"
        );
    }

    #[test]
    fn only_an_upload_is_announced_as_a_package() {
        // `files::upload::audit` records uploads, changes and removals through
        // one function; the feed reports arrivals, and the filter lives here so
        // the hook at the call site stays one unconditional line.
        let events = ServerEvents::new();
        let resource = resource();

        events.package("changed", &resource);
        events.package("removed", &resource);
        assert!(events.since(0).is_empty());

        events.package("uploaded", &resource);
        let published = events.since(0);

        assert_eq!(published.len(), 1);
        assert_eq!(published[0].name(), "package.uploaded");
    }

    #[test]
    fn a_service_heartbeat_is_reported_with_what_the_service_said() {
        let events = ServerEvents::new();

        events.service_status(
            &ServiceName::parse("weather").unwrap(),
            ServiceState::Degraded,
            Some("Upstream is slow."),
        );

        let published = events.since(0);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].name(), "service.status");
        assert!(
            format!("{:?}", published[0]).contains("Degraded"),
            "{:?}",
            published[0]
        );
    }

    #[test]
    fn a_bus_can_be_logged_without_naming_anything_that_crossed_it() {
        let events = ServerEvents::new();
        channel(&events, "ada");

        let printed = format!("{events:?}");

        assert!(printed.contains("published: 1"), "{printed}");
        assert!(!printed.contains("ada"), "{printed}");
    }

    fn resource() -> ResourceRow {
        let now = Utc::now();

        ResourceRow {
            id: 1,
            hash: "abc".into(),
            uid: "res-1".into(),
            name: "map.zip".into(),
            filename: Some("map.zip".into()),
            mime_type: "application/zip".into(),
            size: 12,
            tool: "public".into(),
            creator_uid: None,
            submitter_id: None,
            submitter: Some("ada".into()),
            submission_time: now,
            expiration: None,
            is_mission_package: true,
            groups: Vec::new(),
            mission_name: None,
            latitude: None,
            longitude: None,
            altitude: None,
            remarks: None,
            permissions: None,
            contacts: None,
            download_path: None,
            plugin_class_name: None,
            install_on_enrollment: false,
            deleted_at: None,
            created_at: now,
            keywords: Vec::new(),
        }
    }
}
