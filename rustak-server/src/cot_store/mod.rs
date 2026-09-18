//! Where relayed CoT goes to be read back later.
//!
//! Two stores with different shapes, because the two questions are different.
//! *Where is everybody now?* is one row per uid, overwritten, and lives in
//! `cot_latest` — small, indexed, and what
//! `GET /Marti/api/cot/xml/{uid}` answers from. *What happened between these
//! two times?* is every message ever relayed, appended, and lives in
//! [`store::append_log`](crate::store::append_log) segment files, because fifty
//! devices reporting every two seconds is more writes than the single SQLite
//! writer should ever see.
//!
//! # The routing path never waits for storage
//!
//! [`CotStoreHandle::record`] is `try_send` into a bounded queue and a counter
//! for what did not fit. A database that is briefly busy must never be the
//! reason one client's position report takes longer to reach another client —
//! the stream is the product, and the history is the record of it. A dedicated
//! task drains the queue and batches whole transactions, so the cost per
//! message is amortised over however many arrived together.

pub mod history;
pub mod latest;
pub mod query;
pub mod retention;
pub mod writer;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use rustak_cot::codec::EncodedEvent;
use tokio::sync::mpsc;

use crate::prelude::*;

pub use latest::{LatestRow, latest_event, latest_events, latest_xml};
pub use writer::{CotStoreOptions, start};

/// The stream family CoT history is filed under in `stream_segments`.
pub const STREAM_KIND: &str = "cot";

/// One relayed message, as both stores want it.
///
/// Built from the message in its **relayed** form — `<marti>` already stripped
/// and this server's flow tag already added — so that what is stored is what
/// the recipients were sent, not what the sender wrote.
#[derive(Clone, Debug)]
pub struct CotRecord {
    /// The event uid, which is the key of both stores.
    pub uid: String,
    /// Its CoT type.
    pub kind: String,
    /// The sender's callsign, when the message carried one.
    pub callsign: Option<String>,
    /// The account that sent it.
    pub user_id: Option<UserId>,
    /// The device that sent it.
    pub device_id: Option<DeviceId>,
    /// The sender's channel bit vector at the time, so a replay can answer who
    /// was allowed to see it without re-deriving historical memberships.
    pub group_bits: Vec<u8>,
    /// The event's own three times.
    pub time: DateTime<Utc>,
    /// When the sender says the report starts being valid.
    pub start: DateTime<Utc>,
    /// When it goes stale.
    pub stale: DateTime<Utc>,
    /// Where it says it is.
    pub point: (f64, f64, f64, f64, f64),
    /// The XML the recipients were sent.
    pub xml: String,
    /// The protobuf payload, which is what the history segments hold.
    pub proto: Vec<u8>,
    /// When this server handled it.
    pub received_at: DateTime<Utc>,
}

impl CotRecord {
    /// Builds a record from a message that is about to be relayed.
    pub fn new(encoded: &EncodedEvent, principal: &Principal, device_id: Option<DeviceId>) -> Self {
        let event = encoded.event();

        Self {
            uid: event.uid.clone(),
            kind: event.r#type.clone(),
            callsign: event.callsign().map(str::to_owned),
            user_id: Some(principal.user_id),
            device_id,
            group_bits: principal.groups.to_bytes(),
            time: at(event.time),
            start: at(event.start),
            stale: at(event.stale),
            point: (
                event.point.lat,
                event.point.lon,
                event.point.hae,
                event.point.ce,
                event.point.le,
            ),
            xml: String::from_utf8_lossy(encoded.xml()).into_owned(),
            proto: encoded.proto().to_vec(),
            received_at: Utc::now(),
        }
    }

    /// Whether this message is worth keeping in the history.
    ///
    /// Control and negotiation traffic is `t-x-*` and describes the *connection*
    /// rather than the world; keeping it would fill the segments with pings
    /// nobody will ever read back.
    pub fn is_historic(&self) -> bool {
        !self.kind.starts_with("t-x-")
    }
}

/// A CoT time as a wall clock, falling back to now.
///
/// `CotTime` is milliseconds since the epoch and `chrono` refuses a value
/// outside its own range, which a client can produce by sending a `time` a
/// hundred thousand years from now. Storing "now" for one is better than
/// refusing to record the message: the event's own time is still in the XML.
fn at(time: rustak_cot::CotTime) -> DateTime<Utc> {
    time.to_datetime().unwrap_or_else(Utc::now)
}

/// The routing path's end of the store.
///
/// Cheap to clone; every connection task holds one.
#[derive(Clone, Debug)]
pub struct CotStoreHandle {
    tx: Option<mpsc::Sender<CotRecord>>,
    dropped: Arc<AtomicU64>,
    recorded: Arc<AtomicU64>,
}

impl CotStoreHandle {
    /// A handle that discards everything, for an installation that has turned
    /// recording off.
    ///
    /// Not an `Option<CotStoreHandle>` at every call site: the router would
    /// then have a branch per message for a decision made once at start-up.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            dropped: Arc::new(AtomicU64::new(0)),
            recorded: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Queues a message for storage, without ever waiting.
    pub fn record(&self, record: CotRecord) {
        let Some(tx) = &self.tx else {
            return;
        };

        match tx.try_send(record) {
            Ok(()) => {
                self.recorded.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(record)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;

                // Once per power of two, so a store that has fallen behind says
                // so without the log becoming the reason it stays behind.
                if dropped.is_power_of_two() {
                    warn!(
                        uid = %record.uid,
                        dropped,
                        "The CoT store is behind; messages are being relayed but not recorded."
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// How many messages were queued for storage.
    pub fn recorded(&self) -> u64 {
        self.recorded.load(Ordering::Relaxed)
    }

    /// How many messages were relayed but not recorded.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Whether anything is being recorded at all.
    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// Builds the live handle over a writer's queue.
    pub(crate) fn new(tx: mpsc::Sender<CotRecord>) -> Self {
        Self {
            tx: Some(tx),
            dropped: Arc::new(AtomicU64::new(0)),
            recorded: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::Event;
    use rustak_cot::detail::{Contact, contact::STREAMING_ENDPOINT};

    use super::*;

    fn principal() -> Principal {
        Principal::new(
            UserId::from(3),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
    }

    fn sa() -> EncodedEvent {
        EncodedEvent::new(
            Event::builder("a-f-G-U-C", "UID-A")
                .how("m-g")
                .point(51.5, -0.12)
                .typed(&Contact::new("ALPHA").with_endpoint(STREAMING_ENDPOINT))
                .build(),
        )
    }

    #[test]
    fn a_record_carries_what_the_recipients_were_sent() {
        let record = CotRecord::new(&sa(), &principal(), Some(DeviceId::from(9)));

        assert_eq!(record.uid, "UID-A");
        assert_eq!(record.callsign.as_deref(), Some("ALPHA"));
        assert_eq!(record.user_id, Some(UserId::from(3)));
        assert_eq!(record.device_id, Some(DeviceId::from(9)));
        assert!(record.xml.contains("<event"));
        assert!(!record.proto.is_empty());
        assert!(record.is_historic());
    }

    #[test]
    fn control_traffic_is_not_history() {
        // A segment full of pings is a segment nobody will ever read back.
        let ping = EncodedEvent::new(rustak_cot::msgs::ping("UID-A", rustak_cot::CotTime::now()));
        let record = CotRecord::new(&ping, &principal(), None);

        assert!(!record.is_historic());
    }

    #[test]
    fn a_disabled_store_counts_nothing_and_never_blocks() {
        let handle = CotStoreHandle::disabled();

        handle.record(CotRecord::new(&sa(), &principal(), None));

        assert!(!handle.is_enabled());
        assert_eq!(handle.recorded(), 0);
        assert_eq!(handle.dropped(), 0);
    }

    #[tokio::test]
    async fn a_full_queue_drops_rather_than_holding_the_router_up() {
        let (tx, _rx) = mpsc::channel(1);
        let handle = CotStoreHandle::new(tx);

        handle.record(CotRecord::new(&sa(), &principal(), None));
        handle.record(CotRecord::new(&sa(), &principal(), None));

        assert_eq!(handle.recorded(), 1);
        assert_eq!(handle.dropped(), 1);
    }
}
