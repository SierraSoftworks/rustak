//! The rate-limited half of a feed: what gets published, and what does not.
//!
//! # Why this buffers rather than sends
//!
//! A sidecar does not own its connection — [`Sidecar::tick`](crate::sidecar::Sidecar::tick)
//! and [`on_event`](crate::sidecar::Sidecar::on_event) *return* the events the
//! harness writes, so that nothing is queued into a connection that is down and
//! a plugin that must not lose an event can hold it itself. The publisher fits
//! that shape: [`offer`](FeedPublisher::offer) decides, and
//! [`drain`](FeedPublisher::drain) hands the decision to the harness.
//!
//! ```no_run
//! # use rustak_client::feed::{Affiliation, FeedPublisher, PublishPolicy, Track};
//! # use rustak_cot::Event;
//! # fn example(mut publisher: FeedPublisher, observed: Vec<Track>) -> Vec<Event> {
//! for track in observed {
//!     publisher.offer(track);
//! }
//!
//! publisher.tick();
//! publisher.drain()
//! # }
//! ```

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_cot::Event;
use serde::Serialize;

use super::policy::{COURSE_CHANGE_DEG, SPEED_CHANGE_MPS};
use super::{Affiliation, Area, PublishPolicy, Symbology, Track, distance_m};

/// How often the counters are logged at `info`. Slow on purpose: a feed that
/// logged its rate every tick would be the noisiest thing in the journal.
const REPORT_EVERY: Duration = Duration::from_secs(300);

/// What a feed has done, for the plugin's heartbeat and the operator's logs.
///
/// `offered` is always `published + suppressed`, so a ratio between them says
/// whether the policy is doing anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FeedCounters {
    /// Observations handed to [`FeedPublisher::offer`].
    pub offered: u64,
    /// Observations that became a CoT event.
    pub published: u64,
    /// Observations dropped: outside the area, too soon, or unchanged.
    pub suppressed: u64,
    /// Tracks forgotten: unseen for longer than `stale`, or evicted because the
    /// publisher was full.
    pub expired: u64,
}

/// What we last knew about one track.
#[derive(Debug)]
struct Tracked {
    /// When we last had any report of it, which is what staleness measures.
    seen_at: DateTime<Utc>,
    /// The newest observation time we have accepted, so that an out-of-order
    /// report from a source that reconnected does not move a track backwards.
    observed_at: DateTime<Utc>,
    /// When it was last published, and what was in that publication.
    published: Option<Published>,
}

/// The parts of a publication a later observation is compared against.
#[derive(Debug)]
struct Published {
    at: DateTime<Utc>,
    position: (f64, f64),
    speed_mps: Option<f64>,
    bearing_deg: Option<f64>,
}

/// Turns a stream of observations into a publishable rate of CoT.
///
/// One publisher holds one feed's tracks. It is not `Clone` and holds no locks:
/// a plugin owns one, offers into it from its tick, and drains it in the same
/// breath.
#[derive(Debug)]
pub struct FeedPublisher {
    policy: PublishPolicy,
    affiliation: Affiliation,
    symbology: Symbology,
    area: Area,
    tracks: HashMap<String, Tracked>,
    pending: Vec<Event>,
    counters: FeedCounters,
    reported_at: Option<DateTime<Utc>>,
}

impl FeedPublisher {
    /// A publisher that accepts tracks from anywhere.
    #[must_use]
    pub fn new(policy: PublishPolicy, affiliation: Affiliation) -> Self {
        Self {
            policy,
            affiliation,
            symbology: Symbology::default(),
            area: Area::default(),
            tracks: HashMap::new(),
            pending: Vec::new(),
            counters: FeedCounters::default(),
            reported_at: None,
        }
    }

    /// The same publisher, writing each track's symbol code in an edition of
    /// MIL-STD-2525 as well as its CoT type.
    #[must_use]
    pub fn with_symbology(mut self, symbology: Symbology) -> Self {
        self.symbology = symbology;
        self
    }

    /// Changes the edition for everything published from here on, which is
    /// how an administrator's choice takes effect without reopening a source.
    pub fn set_symbology(&mut self, symbology: Symbology) {
        self.symbology = symbology;
    }

    /// The same publisher, filtering on an area of interest.
    ///
    /// The source subscribes with the area too; this is the second check, for
    /// the upstream that rounds a bounding box outwards or ignores it entirely.
    #[must_use]
    pub fn with_area(mut self, area: Area) -> Self {
        self.area = area;
        self
    }

    /// Offers one observation, answering whether it was published.
    ///
    /// Uses the wall clock; see [`offer_at`](Self::offer_at) for a test that
    /// would rather not.
    pub fn offer(&mut self, track: Track) -> bool {
        self.offer_at(track, Utc::now())
    }

    /// [`offer`](Self::offer), at an instant of the caller's choosing.
    pub fn offer_at(&mut self, track: Track, now: DateTime<Utc>) -> bool {
        self.counters.offered += 1;

        if !self.area.contains(track.position.0, track.position.1) {
            self.counters.suppressed += 1;
            return false;
        }

        let publish = match self.tracks.get(&track.id) {
            // A track nobody has seen before is always worth saying.
            None => true,
            Some(known) => {
                // An observation older than the one we hold is a source that
                // replayed its buffer, not a thing that moved.
                track.observed_at >= known.observed_at && self.changed_enough(known, &track, now)
            }
        };

        let entry = self.tracks.entry(track.id.clone()).or_insert(Tracked {
            seen_at: now,
            observed_at: track.observed_at,
            published: None,
        });
        entry.seen_at = now;
        entry.observed_at = entry.observed_at.max(track.observed_at);

        if !publish {
            self.counters.suppressed += 1;
            return false;
        }

        entry.published = Some(Published {
            at: now,
            position: track.position,
            speed_mps: track.speed_mps,
            bearing_deg: track.bearing_deg(),
        });

        let mut event = track.to_event(self.affiliation, self.policy.stale());
        self.symbology
            .mark(&mut event, track.kind, self.affiliation);
        self.pending.push(event);
        self.counters.published += 1;
        self.enforce_capacity();

        true
    }

    /// Whether this observation says anything the last publication did not.
    fn changed_enough(&self, known: &Tracked, track: &Track, now: DateTime<Utc>) -> bool {
        let Some(published) = &known.published else {
            // Held but never published: a reconnection asked for everything to
            // go out again.
            return true;
        };

        let since = (now - published.at).to_std().unwrap_or_default();

        if since < self.policy.min_interval() {
            return false;
        }

        if since >= self.policy.max_interval() {
            return true;
        }

        if distance_m(published.position, track.position) >= self.policy.min_move_m {
            return true;
        }

        changed_by(published.speed_mps, track.speed_mps, SPEED_CHANGE_MPS)
            || turned_by(
                published.bearing_deg,
                track.bearing_deg(),
                COURSE_CHANGE_DEG,
            )
    }

    /// Expires what has not been seen, evicts what does not fit, and reports.
    ///
    /// Called once per sidecar tick. There is no delete message to send: TAK
    /// clients expire a track by the `stale` on the last event they were given,
    /// so forgetting one here simply stops refreshing it.
    pub fn tick(&mut self) -> usize {
        self.tick_at(Utc::now())
    }

    /// [`tick`](Self::tick), at an instant of the caller's choosing.
    pub fn tick_at(&mut self, now: DateTime<Utc>) -> usize {
        let stale = self.policy.stale();
        let before = self.tracks.len();

        self.tracks
            .retain(|_, held| (now - held.seen_at).to_std().unwrap_or_default() <= stale);

        let expired = before - self.tracks.len();
        self.counters.expired += expired as u64;
        let evicted = self.enforce_capacity();
        self.report(now);

        expired + evicted
    }

    /// Everything the harness should write, and nothing afterwards.
    #[must_use]
    pub fn drain(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.pending)
    }

    /// Marks every held track as unpublished, so the next observation of each
    /// goes out whatever the intervals say.
    ///
    /// This is what a plugin calls on [`SidecarEvent::Connected`](crate::sidecar::SidecarEvent::Connected):
    /// a reconnection is a new subscription, and the server has none of what
    /// was sent down the old one.
    pub fn refresh_all(&mut self) {
        for held in self.tracks.values_mut() {
            held.published = None;
        }
    }

    /// What this feed has done so far.
    #[must_use]
    pub fn counters(&self) -> FeedCounters {
        self.counters
    }

    /// How many tracks are currently held.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.tracks.len()
    }

    /// The policy this publisher was built with.
    #[must_use]
    pub fn policy(&self) -> &PublishPolicy {
        &self.policy
    }

    /// Drops the least recently seen tracks until the publisher fits.
    fn enforce_capacity(&mut self) -> usize {
        let over = self.tracks.len().saturating_sub(self.policy.max_tracks);
        if over == 0 {
            return 0;
        }

        let mut seen: Vec<(DateTime<Utc>, String)> = self
            .tracks
            .iter()
            .map(|(id, held)| (held.seen_at, id.clone()))
            .collect();
        seen.sort_unstable();

        for (_, id) in seen.into_iter().take(over) {
            self.tracks.remove(&id);
        }

        self.counters.expired += over as u64;

        over
    }

    /// Logs the counters, no more often than [`REPORT_EVERY`].
    fn report(&mut self, now: DateTime<Utc>) {
        let due = match self.reported_at {
            None => true,
            Some(last) => (now - last).to_std().unwrap_or_default() >= REPORT_EVERY,
        };

        if !due {
            return;
        }

        self.reported_at = Some(now);
        tracing::info!(
            tracked = self.tracks.len(),
            offered = self.counters.offered,
            published = self.counters.published,
            suppressed = self.counters.suppressed,
            expired = self.counters.expired,
            "The feed is publishing.",
        );
    }
}

/// Whether two optional readings differ by at least `threshold`.
fn changed_by(before: Option<f64>, after: Option<f64>, threshold: f64) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => (after - before).abs() >= threshold,
        // A reading that appeared or disappeared is news in itself.
        (None, None) => false,
        _ => true,
    }
}

/// [`changed_by`] for a bearing, where 359° and 1° are two degrees apart.
fn turned_by(before: Option<f64>, after: Option<f64>, threshold: f64) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => {
            let delta = (after - before).rem_euclid(360.0);

            delta.min(360.0 - delta) >= threshold
        }
        (None, None) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{AircraftClass, TrackKind, VesselClass};

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    fn vessel(id: &str, position: (f64, f64), at: DateTime<Utc>) -> Track {
        Track::new(id, TrackKind::Vessel(VesselClass::Merchant), position, at)
            .with_velocity(6.2, 271.5)
    }

    fn publisher() -> FeedPublisher {
        FeedPublisher::new(PublishPolicy::default(), Affiliation::Unknown)
    }

    #[test]
    fn a_track_nobody_has_seen_is_always_published() {
        let mut publisher = publisher();

        assert!(publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0)));

        let published = publisher.drain();

        assert_eq!(published.len(), 1);
        assert_eq!(published[0].uid, "AIS-1");
        assert_eq!(published[0].r#type, "a-u-S-X-M");
        assert!(publisher.drain().is_empty(), "draining takes them away");
        assert_eq!(publisher.tracked(), 1);
    }

    #[test]
    fn the_same_track_ten_times_in_a_second_is_published_once() {
        // The whole point of the policy: a thousand ships reporting every two
        // seconds must not be a thousand messages a second.
        let mut publisher = publisher();

        for tenth in 0..10 {
            let now = at(0) + chrono::Duration::milliseconds(tenth * 100);
            publisher.offer_at(vessel("AIS-1", (51.9, 4.1), now), now);
        }

        assert_eq!(publisher.drain().len(), 1);
        assert_eq!(
            publisher.counters(),
            FeedCounters {
                offered: 10,
                published: 1,
                suppressed: 9,
                expired: 0,
            }
        );
    }

    #[test]
    fn a_track_that_moved_far_enough_is_published_again() {
        let mut publisher = publisher();

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0));

        // 25 m is the default; a thousandth of a degree of latitude is 111 m.
        assert!(
            publisher.offer_at(vessel("AIS-1", (51.901, 4.1), at(10)), at(10)),
            "a ship that moved 111 m has moved",
        );
        // Five centimetres, well inside the floor.
        assert!(
            !publisher.offer_at(vessel("AIS-1", (51.9010005, 4.1), at(20)), at(20)),
            "a ship at a berth has not",
        );
        assert_eq!(publisher.drain().len(), 2);
    }

    #[test]
    fn nothing_is_published_twice_inside_the_minimum_interval_however_far_it_moved() {
        let mut publisher = publisher();

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0));

        assert!(
            !publisher.offer_at(vessel("AIS-1", (52.9, 5.1), at(2)), at(2)),
            "a hundred kilometres in two seconds is still two seconds",
        );
        assert!(publisher.offer_at(vessel("AIS-1", (52.9, 5.1), at(6)), at(6)));
    }

    #[test]
    fn a_motionless_track_is_republished_before_it_goes_stale() {
        // Otherwise a moored vessel drops off every map two minutes after it
        // moors, and comes back only when it sails.
        let mut publisher = publisher();

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0));
        let _ = publisher.drain();

        assert!(!publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(30)), at(30)));
        assert!(
            publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(61)), at(61)),
            "the maximum interval has passed",
        );
        assert_eq!(publisher.drain().len(), 1);
    }

    #[test]
    fn a_turn_or_a_change_of_speed_is_published_without_moving_far() {
        let mut publisher = publisher();
        let anchored = |bearing: f64, speed: f64, now| {
            Track::new(
                "AIS-1",
                TrackKind::Vessel(VesselClass::Merchant),
                (51.9, 4.1),
                now,
            )
            .with_velocity(speed, bearing)
        };

        publisher.offer_at(anchored(10.0, 6.0, at(0)), at(0));

        assert!(
            publisher.offer_at(anchored(350.0, 6.0, at(10)), at(10)),
            "twenty degrees across north is a turn, not a 340-degree swing",
        );
        assert!(
            !publisher.offer_at(anchored(355.0, 6.0, at(20)), at(20)),
            "five degrees is not",
        );
        assert!(
            publisher.offer_at(anchored(355.0, 0.0, at(30)), at(30)),
            "a vessel that stopped is news",
        );
    }

    #[test]
    fn an_out_of_order_report_does_not_move_a_track_backwards() {
        // A source that reconnected and replayed its buffer must not drag a
        // track back to where it was a minute ago.
        let mut publisher = publisher();

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(60)), at(60));

        assert!(!publisher.offer_at(vessel("AIS-1", (51.8, 4.1), at(0)), at(70)));
        assert_eq!(publisher.drain().len(), 1);
    }

    #[test]
    fn a_track_outside_the_area_is_never_published() {
        let mut publisher = publisher().with_area(Area::Circle {
            lat: 51.9,
            lon: 4.1,
            radius_km: 20.0,
        });

        assert!(publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0)));
        assert!(!publisher.offer_at(vessel("AIS-2", (48.8, 2.3), at(0)), at(0)));
        assert_eq!(publisher.tracked(), 1, "and it is not even remembered");
        assert_eq!(publisher.counters().suppressed, 1);
    }

    #[test]
    fn a_track_nobody_has_reported_for_longer_than_stale_is_forgotten() {
        let mut publisher = publisher();

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0));
        publisher.offer_at(vessel("AIS-2", (51.9, 4.2), at(0)), at(0));
        let _ = publisher.drain();

        assert_eq!(publisher.tick_at(at(60)), 0);

        publisher.offer_at(vessel("AIS-2", (51.9, 4.2), at(100)), at(100));
        let _ = publisher.drain();

        assert_eq!(publisher.tick_at(at(130)), 1, "AIS-1 stopped reporting");
        assert_eq!(publisher.tracked(), 1);
        assert_eq!(publisher.counters().expired, 1);
        assert!(
            publisher.drain().is_empty(),
            "expiry sends nothing: clients expire it themselves",
        );
    }

    #[test]
    fn the_least_recently_seen_track_is_evicted_when_the_publisher_is_full() {
        let mut publisher = FeedPublisher::new(
            PublishPolicy {
                max_tracks: 2,
                ..PublishPolicy::default()
            },
            Affiliation::Unknown,
        );

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0));
        publisher.offer_at(vessel("AIS-2", (51.9, 4.2), at(1)), at(1));
        publisher.offer_at(vessel("AIS-3", (51.9, 4.3), at(2)), at(2));

        assert_eq!(publisher.tracked(), 2);
        assert_eq!(publisher.counters().expired, 1);
    }

    #[test]
    fn a_reconnection_republishes_everything_the_server_no_longer_knows() {
        let mut publisher = publisher();

        publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(0)), at(0));
        let _ = publisher.drain();

        assert!(!publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(1)), at(1)));

        publisher.refresh_all();

        assert!(
            publisher.offer_at(vessel("AIS-1", (51.9, 4.1), at(2)), at(2)),
            "a reopened connection is a new subscription",
        );
    }

    #[test]
    fn the_stale_attribute_comes_from_the_policy() {
        let mut publisher = FeedPublisher::new(
            PublishPolicy::default().with_stale(Duration::from_secs(90)),
            Affiliation::Friend,
        );

        publisher.offer_at(
            Track::new(
                "ADSB-3c6444",
                TrackKind::Aircraft(AircraftClass::CivilFixedWing),
                (51.4, -0.45),
                at(0),
            ),
            at(0),
        );

        let published = publisher.drain();
        let event = published.first().expect("the aircraft");

        assert_eq!(event.r#type, "a-f-A-C-F");
        assert_eq!(event.stale.millis() - event.time.millis(), 90_000);
    }
}
