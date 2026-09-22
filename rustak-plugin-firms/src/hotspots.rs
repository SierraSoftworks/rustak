//! Which detections are on the map, and when each is said again.
//!
//! [`rustak_client::feed::FeedPublisher`] is built for things that move: it
//! decides by distance, course and speed, and ages a track from its last
//! report. A fire detection does none of that. It is one observation, it never
//! moves, and it is worth exactly as long as it is recent. So this is the
//! plugin's own, much smaller, publisher:
//!
//! - a detection is **filtered** by area, confidence, power and age, then held
//!   under its deterministic uid, so a poll that returns the same rows again
//!   adds nothing;
//! - it is **published** once, then **republished** every `republish`, because
//!   a server replays only one event per connection to a device that joins
//!   later, and a fire map that is empty for latecomers is not a fire map;
//! - at most `max_per_tick` detections leave on any one tick, newest first, so
//!   a large area on a bad day is a steady stream rather than one burst;
//! - it is **forgotten** `max_age` after the satellite saw it, which is also
//!   the `stale` every client was given. No delete is ever sent.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_client::feed::Area;
use rustak_core::config::duration;
use rustak_core::prelude::*;
use rustak_cot::Event;

use crate::mapping::{self, Display};
use crate::wire::{Confidence, Detection};

/// How far ahead of our clock an acquisition may be and still be believed.
const CLOCK_SKEW: Duration = Duration::from_secs(3_600);

/// `[settings.filter]` — which detections are worth drawing.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// The least confident detection to draw. Default: `low`, which is all of
    /// them. A detection that does not say is treated as `nominal`.
    #[serde(default)]
    pub min_confidence: Confidence,

    /// The least fire radiative power to draw, in megawatts. Default: 0. A
    /// detection that does not say passes only while this is 0.
    #[serde(default)]
    pub min_frp_mw: f64,
}

/// `[settings.publish]` — how long a detection lives and how often it is said.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    /// How long after its acquisition a detection stays on a map.
    #[serde(default = "default_max_age", with = "duration::humane")]
    pub max_age: chrono::Duration,

    /// How often a detection that is still on the map is said again.
    #[serde(default = "default_republish", with = "duration::humane")]
    pub republish: chrono::Duration,

    /// How many detections are held before the oldest are dropped.
    #[serde(default = "default_max_detections")]
    pub max_detections: usize,

    /// How many detections may be published on one tick.
    #[serde(default = "default_max_per_tick")]
    pub max_per_tick: usize,
}

impl Default for Publish {
    fn default() -> Self {
        Self {
            max_age: default_max_age(),
            republish: default_republish(),
            max_detections: default_max_detections(),
            max_per_tick: default_max_per_tick(),
        }
    }
}

impl Publish {
    /// [`max_age`](Self::max_age) as the standard library spells it.
    #[must_use]
    pub fn max_age(&self) -> Duration {
        self.max_age
            .to_std()
            .unwrap_or_else(|_| Duration::from_secs(86_400))
    }
}

fn default_max_age() -> chrono::Duration {
    chrono::Duration::hours(24)
}

fn default_republish() -> chrono::Duration {
    chrono::Duration::minutes(10)
}

const fn default_max_detections() -> usize {
    5_000
}

const fn default_max_per_tick() -> usize {
    500
}

/// What this publisher has done, for the heartbeat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counters {
    /// Detections handed to [`Hotspots::offer`].
    pub offered: u64,
    /// Detections published for the first time.
    pub published: u64,
    /// Publications of a detection that had been published before.
    pub republished: u64,
    /// Detections dropped: filtered out, or already known.
    pub suppressed: u64,
    /// Detections forgotten: older than `max_age`, or evicted for room.
    pub expired: u64,
}

/// One detection, and when it last went out.
#[derive(Debug)]
struct Known {
    detection: Detection,
    published_at: Option<DateTime<Utc>>,
}

/// The detections this sidecar is carrying.
#[derive(Debug)]
pub struct Hotspots {
    area: Area,
    filter: Filter,
    display: Display,
    publish: Publish,
    known: HashMap<String, Known>,
    counters: Counters,
}

impl Hotspots {
    /// An empty map.
    #[must_use]
    pub fn new(area: Area, filter: Filter, display: Display, publish: Publish) -> Self {
        Self {
            area,
            filter,
            display,
            publish,
            known: HashMap::new(),
            counters: Counters::default(),
        }
    }

    /// Takes a detection, and answers whether it was new and wanted.
    pub fn offer(&mut self, detection: Detection) -> bool {
        self.offer_at(detection, Utc::now())
    }

    /// [`offer`](Self::offer), at an instant of the caller's choosing.
    pub fn offer_at(&mut self, detection: Detection, now: DateTime<Utc>) -> bool {
        self.counters.offered += 1;

        let uid = mapping::uid(&detection);

        if !self.wanted(&detection, now) || self.known.contains_key(&uid) {
            self.counters.suppressed += 1;

            return false;
        }

        self.known.insert(
            uid,
            Known {
                detection,
                published_at: None,
            },
        );

        true
    }

    /// Whether this detection passes every filter.
    fn wanted(&self, detection: &Detection, now: DateTime<Utc>) -> bool {
        let age = now - detection.acquired_at;
        let fresh = age < self.publish.max_age && -age <= skew();
        let confident =
            detection.confidence.unwrap_or(Confidence::Nominal) >= self.filter.min_confidence;
        let powerful = match detection.frp_mw {
            Some(frp) => frp >= self.filter.min_frp_mw,
            None => self.filter.min_frp_mw <= 0.0,
        };

        fresh && confident && powerful && self.area.contains(detection.lat, detection.lon)
    }

    /// Forgets what is too old or too much, and answers how many that was.
    pub fn tick(&mut self) -> usize {
        self.tick_at(Utc::now())
    }

    /// [`tick`](Self::tick), at an instant of the caller's choosing.
    pub fn tick_at(&mut self, now: DateTime<Utc>) -> usize {
        let before = self.known.len();
        let max_age = self.publish.max_age;

        self.known
            .retain(|_, known| now - known.detection.acquired_at < max_age);

        if self.known.len() > self.publish.max_detections {
            let mut oldest: Vec<(DateTime<Utc>, String)> = self
                .known
                .iter()
                .map(|(uid, known)| (known.detection.acquired_at, uid.clone()))
                .collect();
            oldest.sort();

            let excess = self.known.len() - self.publish.max_detections;

            for (_, uid) in oldest.into_iter().take(excess) {
                self.known.remove(&uid);
            }
        }

        let forgotten = before - self.known.len();
        self.counters.expired += forgotten as u64;

        forgotten
    }

    /// The events that are due: never-published detections first, newest
    /// acquisition first, then the ones whose `republish` has come round.
    pub fn drain(&mut self) -> Vec<Event> {
        self.drain_at(Utc::now())
    }

    /// [`drain`](Self::drain), at an instant of the caller's choosing.
    pub fn drain_at(&mut self, now: DateTime<Utc>) -> Vec<Event> {
        let republish = self.publish.republish;
        let mut due: Vec<(Option<DateTime<Utc>>, DateTime<Utc>, String)> = self
            .known
            .iter()
            .filter(|(_, known)| known.published_at.is_none_or(|at| now - at >= republish))
            .map(|(uid, known)| (known.published_at, known.detection.acquired_at, uid.clone()))
            .collect();

        // `None` sorts before `Some`, and an older publication before a newer.
        due.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        due.truncate(self.publish.max_per_tick.max(1));

        let max_age = self.publish.max_age();
        let mut events = Vec::with_capacity(due.len());

        for (_, _, uid) in due {
            let Some(known) = self.known.get_mut(&uid) else {
                continue;
            };

            match known.published_at.replace(now) {
                Some(_) => self.counters.republished += 1,
                None => self.counters.published += 1,
            }

            events.extend(mapping::events(&known.detection, &self.display, max_age));
        }

        events
    }

    /// Makes every detection due again: a reopened connection is a new
    /// subscription, and the server has none of what went down the old one.
    pub fn refresh_all(&mut self) {
        for known in self.known.values_mut() {
            known.published_at = None;
        }
    }

    /// What has been offered, published, republished, suppressed and expired.
    #[must_use]
    pub const fn counters(&self) -> Counters {
        self.counters
    }

    /// How many detections are on the map right now.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.known.len()
    }

    /// How many detections have never been published: the backlog
    /// `max_per_tick` is working through.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.known
            .values()
            .filter(|known| known.published_at.is_none())
            .count()
    }
}

/// [`CLOCK_SKEW`] as chrono spells it.
fn skew() -> chrono::Duration {
    chrono::Duration::from_std(CLOCK_SKEW).unwrap_or_else(|_| chrono::Duration::zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-09-22T14:00:00Z".parse().expect("an instant")
    }

    fn minutes(count: i64) -> chrono::Duration {
        chrono::Duration::minutes(count)
    }

    /// A detection `age` minutes old, `offset` hundredths of a degree along.
    fn detection(offset: i32, age: i64) -> Detection {
        Detection {
            confidence: Some(Confidence::Nominal),
            frp_mw: Some(20.0),
            ..Detection::at(40.0 + f64::from(offset) / 100.0, -8.0, now() - minutes(age))
        }
    }

    fn hotspots(filter: Filter, publish: Publish) -> Hotspots {
        let area = Area::Circle {
            lat: 40.0,
            lon: -8.0,
            radius_km: 100.0,
        };

        Hotspots::new(area, filter, Display::default(), publish)
    }

    #[test]
    fn a_detection_is_published_once_however_often_the_feed_returns_it() {
        let mut hotspots = hotspots(Filter::default(), Publish::default());

        assert!(hotspots.offer_at(detection(0, 30), now()));
        assert_eq!(hotspots.drain_at(now()).len(), 1);

        // The next poll returns the same row: nothing new, nothing due.
        assert!(!hotspots.offer_at(detection(0, 30), now() + minutes(5)));
        assert!(hotspots.drain_at(now() + minutes(5)).is_empty());
        assert_eq!(hotspots.counters().published, 1);
        assert_eq!(hotspots.counters().suppressed, 1);
    }

    #[test]
    fn a_detection_is_said_again_for_whoever_joined_since() {
        let mut hotspots = hotspots(Filter::default(), Publish::default());
        hotspots.offer_at(detection(0, 30), now());
        let first = hotspots.drain_at(now());

        let again = hotspots.drain_at(now() + minutes(10));

        assert_eq!(again.len(), 1);
        assert_eq!(again[0].uid, first[0].uid, "the same object, refreshed");
        assert_eq!(again[0].stale, first[0].stale, "and no longer-lived for it");
        assert_eq!(hotspots.counters().republished, 1);
    }

    #[test]
    fn what_the_operator_filtered_out_never_reaches_the_map() {
        let strict = Filter {
            min_confidence: Confidence::Nominal,
            min_frp_mw: 10.0,
        };
        let cases = [
            (
                "outside the area",
                Detection {
                    lon: 10.0,
                    ..detection(0, 30)
                },
            ),
            (
                "too unsure",
                Detection {
                    confidence: Some(Confidence::Low),
                    ..detection(0, 30)
                },
            ),
            (
                "too weak",
                Detection {
                    frp_mw: Some(2.0),
                    ..detection(0, 30)
                },
            ),
            (
                "power unknown",
                Detection {
                    frp_mw: None,
                    ..detection(0, 30)
                },
            ),
            ("older than max_age", detection(0, 25 * 60)),
            ("from the future", detection(0, -3 * 60)),
        ];

        for (why, unwanted) in cases {
            let mut hotspots = hotspots(strict, Publish::default());

            assert!(!hotspots.offer_at(unwanted, now()), "{why}");
            assert_eq!(hotspots.tracked(), 0, "{why}");
        }

        let mut hotspots = hotspots(strict, Publish::default());

        assert!(
            hotspots.offer_at(
                Detection {
                    confidence: None,
                    ..detection(0, 30)
                },
                now()
            ),
            "a detection that does not say how sure it is counts as nominal",
        );
    }

    #[test]
    fn a_detection_is_forgotten_when_its_stale_time_passes() {
        let mut hotspots = hotspots(Filter::default(), Publish::default());
        hotspots.offer_at(detection(0, 23 * 60), now());

        assert_eq!(hotspots.tick_at(now() + minutes(30)), 0);
        assert_eq!(hotspots.tick_at(now() + minutes(61)), 1);
        assert_eq!(hotspots.tracked(), 0);
        assert_eq!(hotspots.counters().expired, 1);
    }

    #[test]
    fn a_bad_day_leaves_in_batches_newest_first() {
        let publish = Publish {
            max_per_tick: 2,
            ..Publish::default()
        };
        let mut hotspots = hotspots(Filter::default(), publish);

        for offset in 0..5 {
            hotspots.offer_at(detection(offset, 60 - i64::from(offset)), now());
        }

        let first = hotspots.drain_at(now());

        assert_eq!(first.len(), 2);
        assert_eq!(
            first[0].uid,
            mapping::uid(&detection(4, 56)),
            "the newest leads"
        );
        assert_eq!(hotspots.pending(), 3);
        assert_eq!(hotspots.drain_at(now()).len(), 2);
        assert_eq!(hotspots.drain_at(now()).len(), 1);
        assert!(hotspots.drain_at(now()).is_empty());
    }

    #[test]
    fn the_oldest_detections_make_room_when_the_map_is_full() {
        let publish = Publish {
            max_detections: 2,
            ..Publish::default()
        };
        let mut hotspots = hotspots(Filter::default(), publish);

        for offset in 0..3 {
            hotspots.offer_at(detection(offset, 60 - i64::from(offset)), now());
        }

        assert_eq!(hotspots.tick_at(now()), 1);

        let kept: Vec<String> = hotspots
            .drain_at(now())
            .into_iter()
            .map(|e| e.uid)
            .collect();

        assert!(!kept.contains(&mapping::uid(&detection(0, 60))), "{kept:?}");
    }

    #[test]
    fn a_reconnection_makes_everything_due_again() {
        let mut hotspots = hotspots(Filter::default(), Publish::default());
        hotspots.offer_at(detection(0, 30), now());
        let _ = hotspots.drain_at(now());

        hotspots.refresh_all();

        assert_eq!(hotspots.drain_at(now()).len(), 1);
    }

    #[test]
    fn both_shapes_count_as_one_detection_against_the_batch() {
        let display = Display {
            shape: crate::mapping::Shape::Both,
            ..Display::default()
        };
        let mut hotspots = Hotspots::new(
            Area::default(),
            Filter::default(),
            display,
            Publish {
                max_per_tick: 1,
                ..Publish::default()
            },
        );
        hotspots.offer_at(
            Detection {
                scan_km: Some(0.4),
                track_km: Some(0.4),
                ..detection(0, 30)
            },
            now(),
        );

        assert_eq!(
            hotspots.drain_at(now()).len(),
            2,
            "a marker and its footprint"
        );
    }
}
