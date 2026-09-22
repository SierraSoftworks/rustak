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

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use parking_lot::Mutex;
use rustak_api::event::{
    ChannelEvent, ClientEvent, ConfigValidationEvent, MissionEvent, PackageEvent, ServerEvent,
    ServerEventPayload, ServiceEvent,
};
use rustak_api::{ServiceName, ServiceState};
use tokio::sync::broadcast;

use crate::db::repos::ResourceRow;
use crate::prelude::*;
use crate::stream::live::{ConnectionChange, ConnectionWatcher};
use crate::stream::mission_notify::MissionNotice;

use super::visibility::Audience;

/// How many recent events are kept for `Last-Event-ID` to resume from.
///
/// Also the broadcast channel's depth, so that "too far behind to resume" and
/// "too far behind to keep receiving" are the same distance: a consumer that
/// survives a lag can always resume from the ring.
pub const RING: usize = 256;

/// The `action` [`files::upload::audit`](crate::files::upload::audit) uses for
/// a file that has just arrived, which is the only one the feed reports.
const UPLOADED: &str = "uploaded";

/// One published event, with the rule that says who may be shown it.
///
/// The bus and the ring carry this rather than a bare [`ServerEvent`], so that
/// the decision is made where the channel list is still in hand and every
/// subscriber is filtered against the same recorded answer — including when it
/// is replayed from the ring to a consumer resuming from `Last-Event-ID`.
#[derive(Clone, Debug)]
pub struct PublishedEvent {
    /// What goes on the wire, unchanged.
    pub event: ServerEvent,

    /// Who may be shown it.
    pub audience: Audience,
}

/// The bus every server event is published on.
///
/// Cheap to clone — one [`Arc`] — and safe to hold: publishing into a bus with
/// no subscribers is a counter increment and a push onto the ring.
#[derive(Clone)]
pub struct ServerEvents {
    inner: Arc<Inner>,
}

struct Inner {
    sender: broadcast::Sender<Arc<PublishedEvent>>,
    next_id: AtomicU64,
    /// Behind an [`Arc`] so that a resume or a lagged refill copies pointers
    /// rather than up to 256 payloads while holding the lock every publisher —
    /// including `Hub::announce`, on a connecting client's own task — waits on
    /// (R-01 M12).
    recent: Mutex<VecDeque<Arc<PublishedEvent>>>,
    /// Accounts whose open feeds must re-authorize immediately, rather than
    /// waiting for the next periodic check.
    invalidated: broadcast::Sender<Username>,
    /// How many feeds each service holds open right now, which is whether there
    /// is anything to carry a request *to* it. See [`ServerEvents::attach`].
    attached: Mutex<HashMap<ServiceName, usize>>,
}

/// One service's open feed, counted until this is dropped.
pub struct Attachment {
    events: ServerEvents,
    service: ServiceName,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let mut attached = self.events.inner.attached.lock();

        if let Some(count) = attached.get_mut(&self.service) {
            *count -= 1;

            if *count == 0 {
                attached.remove(&self.service);
            }
        }
    }
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
        let (invalidated, _) = broadcast::channel(RING);

        Self {
            inner: Arc::new(Inner {
                sender,
                next_id: AtomicU64::new(0),
                recent: Mutex::new(VecDeque::with_capacity(RING)),
                invalidated,
                attached: Mutex::new(HashMap::new()),
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
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<PublishedEvent>> {
        self.inner.sender.subscribe()
    }

    /// How many feeds are open right now.
    ///
    /// `web::api::events` caps them: a broadcast channel keeps one `RING`-slot
    /// buffer per receiver, so unbounded subscribers is unbounded memory.
    pub fn subscribers(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    /// Records that `service` has a feed open, for as long as the answer lives.
    ///
    /// The feed is the only way this server has of reaching a sidecar, which
    /// dials us and never the other way round, so "is there a feed" is "can it
    /// be asked anything" — see [`crate::plugins::validation`].
    pub fn attach(&self, service: &ServiceName) -> Attachment {
        *self
            .inner
            .attached
            .lock()
            .entry(service.clone())
            .or_default() += 1;

        Attachment {
            events: self.clone(),
            service: service.clone(),
        }
    }

    /// Whether `service` has a feed open right now.
    pub fn is_attached(&self, service: &ServiceName) -> bool {
        self.inner.attached.lock().contains_key(service)
    }

    /// Starts hearing about accounts whose open feeds must re-authorize.
    ///
    /// Disabling an account or revoking the token behind it has to take the
    /// feed away *now* rather than at the next periodic check (R-01 H4/H5), and
    /// the feed is an HTTP response rather than a registered connection, so
    /// there is nothing for `LiveState` to close. This is how it is told.
    pub fn invalidations(&self) -> broadcast::Receiver<Username> {
        self.inner.invalidated.subscribe()
    }

    /// Tells every open feed belonging to `username` to re-authorize now.
    ///
    /// Never fails: no open feed is the ordinary case.
    pub fn invalidate(&self, username: &Username) {
        let _ = self.inner.invalidated.send(username.clone());
    }

    /// Everything still held that happened after `id`, oldest first.
    ///
    /// This is `Last-Event-ID` resume. An `id` older than the ring gives
    /// everything the ring holds, which is the honest answer to "I have been
    /// away too long": the consumer sees a gap in the ids and can re-read
    /// whatever it keeps state about.
    pub fn since(&self, id: u64) -> Vec<Arc<PublishedEvent>> {
        self.inner
            .recent
            .lock()
            .iter()
            .filter(|published| published.event.id > id)
            .map(Arc::clone)
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
    pub fn publish(&self, payload: ServerEventPayload, audience: Audience) -> ServerEvent {
        let published = Arc::new(PublishedEvent {
            event: ServerEvent {
                id: self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1,
                at: Utc::now(),
                payload,
            },
            audience,
        });

        {
            let mut recent = self.inner.recent.lock();

            if recent.len() == RING {
                recent.pop_front();
            }

            recent.push_back(Arc::clone(&published));
        }

        // `Err` means nobody is subscribed, which is not a failure.
        let _ = self.inner.sender.send(Arc::clone(&published));

        published.event.clone()
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

            // A connection is announced to the readers it could have reached
            // over the stream itself, which is the rule `Hub::snapshot_for`
            // applies to the client listing (R-01 H3). An incognito client is
            // announced to administrators alone — they see it in
            // `GET /api/v1/clients` too — because appearing unasked is exactly
            // what going incognito is a refusal of (R-01 M14).
            let audience = if connection.incognito {
                Audience::Administrators
            } else {
                Audience::Reachable(Arc::clone(&connection.groups))
            };

            events.publish(
                match change {
                    ConnectionChange::Joined(_) => ServerEventPayload::ClientConnected(client),
                    ConnectionChange::Left(_) => ServerEventPayload::ClientDisconnected(client),
                },
                audience,
            );
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

        // The same rule the mission listing applies, and the same one the CoT
        // stream's own broadcast arm applies: a caller who may receive from one
        // of the mission's channels may know it changed, and nobody else may
        // (R-01 H3). An empty list is `__ANON__`, which is what makes a mission
        // nobody scoped visible to the installation rather than to nobody.
        let audience = Audience::Channels(
            mission
                .groups
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
        );

        self.publish(
            ServerEventPayload::MissionChanged(MissionEvent {
                name: mission.name.clone(),
                guid: Some(mission.guid.clone()),
                change: change_type(notice).to_string(),
                author_uid: notice.author_uid().map(ToOwned::to_owned),
            }),
            audience,
        );
    }

    /// `channel.changed`: an account's channels were re-evaluated.
    pub fn channel_changed(&self, username: &Username) {
        // Whose channels changed is a fact about one account. An administrator
        // watching the installation needs it; another tenant's sidecar does not,
        // and the account itself does, so that a sidecar re-reads its own
        // memberships after an operator edits them.
        self.publish(
            ServerEventPayload::ChannelChanged(ChannelEvent {
                username: username.to_string(),
            }),
            Audience::Account(username.clone()),
        );
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

        // The hash is the download handle for `GET /Marti/sync/content?hash=`,
        // so this event is at least as sensitive as the package itself and is
        // filtered by exactly the rule `packages::readable` applies (R-01 H3).
        self.publish(
            ServerEventPayload::PackageUploaded(PackageEvent {
                uid: resource.uid.clone(),
                name: resource.name.clone(),
                hash: resource.hash.clone(),
                size: resource.size,
                mission_package: resource.is_mission_package,
                submitter: resource.submitter.clone(),
            }),
            Audience::Channels(resource.groups.clone()),
        );
    }

    /// `service.config.validate`: a service is asked to check a candidate.
    ///
    /// To that service alone, and carrying an id rather than the candidate —
    /// which may hold a secret, and this bus keeps a ring of what it carried.
    pub fn config_validation(&self, service: &ServiceName, id: uuid::Uuid) {
        self.publish(
            ServerEventPayload::ConfigValidationRequested(ConfigValidationEvent {
                service: service.clone(),
                request_id: id,
            }),
            Audience::Service(service.clone()),
        );
    }

    /// `service.status`: a registered service said how it is doing.
    pub fn service_status(&self, name: &ServiceName, state: ServiceState, message: Option<&str>) {
        // `GET /api/v1/services` is administrative precisely so that one
        // sidecar cannot enumerate the fleet (R-01 M16); the feed must not be
        // the way around it. A service still hears about itself.
        self.publish(
            ServerEventPayload::ServiceStatus(ServiceEvent {
                name: name.clone(),
                state,
                message: message.map(|said| bounded(said).to_string()),
            }),
            Audience::Service(name.clone()),
        );
    }
}

/// How much of a service's own status message is carried on the bus.
///
/// `Heartbeat.message` has no length bound of its own and the control API is
/// deliberately unrate-limited, so a ring of 256 events was a quarter of a
/// gigabyte one service token could pin (R-01 M12). The events are behind an
/// [`Arc`] now, so each one is resident once rather than twice — and a sidecar
/// reporting why it is unhealthy still has nothing useful to say past this.
const MESSAGE_LIMIT: usize = 512;

/// `message`, truncated at [`MESSAGE_LIMIT`] on a character boundary.
fn bounded(message: &str) -> &str {
    if message.len() <= MESSAGE_LIMIT {
        return message;
    }

    let mut end = MESSAGE_LIMIT;

    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }

    &message[..end]
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
        events.publish(
            ServerEventPayload::ChannelChanged(ChannelEvent {
                username: username.to_string(),
            }),
            Audience::Everyone,
        )
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

        assert_eq!(received.event.name(), "channel.changed");
        assert_eq!(
            received.event.id, 2,
            "the id is the bus's, not the receiver's"
        );
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
        assert_eq!(resumed[0].event.id, 2);
        assert_eq!(resumed[1].event.id, 3);
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
            everything[0].event.id, 11,
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
        assert_eq!(published[0].event.name(), "package.uploaded");
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
        assert_eq!(published[0].event.name(), "service.status");
        assert!(
            format!("{:?}", published[0]).contains("Degraded"),
            "{:?}",
            published[0]
        );
    }

    #[test]
    fn a_services_own_words_are_bounded_before_they_reach_the_ring() {
        // R-01 M12. The message is a sidecar's, has no length of its own, and
        // is kept for 256 events.
        let events = ServerEvents::new();
        let said = "é".repeat(MESSAGE_LIMIT);

        events.service_status(
            &ServiceName::parse("weather").unwrap(),
            ServiceState::Degraded,
            Some(&said),
        );

        let published = events.since(0);
        let rendered = serde_json::to_string(&published[0].event).unwrap();

        assert!(rendered.len() < said.len(), "{}", rendered.len());
        assert!(
            rendered.contains('é'),
            "truncating on a character boundary keeps it readable",
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
