//! What goes out, and how often.
//!
//! An outage sits still, so there is nothing to rate-limit by distance: a
//! marker is published when it is new, when anything ESB says about it has
//! changed, and once per `refresh` so that it outlives its own `stale`. An
//! outage that leaves ESB's list is simply no longer refreshed — **no delete
//! is ever sent**, and every client drops the marker when its `stale` passes.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_client::feed::FeedCounters;
use rustak_core::prelude::*;
use rustak_cot::Event;

use crate::outage::{Outage, OutageKind};
use crate::scope::Scope;

/// What is on the map, for the heartbeat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Unplanned faults.
    pub fault: usize,
    /// Planned works.
    pub planned: usize,
    /// Recent restorations.
    pub restored: usize,
    /// Anything else ESB listed.
    pub other: usize,
    /// Customers off supply across the faults.
    pub customers: u64,
}

impl Summary {
    /// Every marker on the map.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.fault + self.planned + self.restored + self.other
    }
}

#[derive(Debug)]
struct Held {
    outage: Outage,
    /// When this marker is next published whether or not it has changed.
    due: DateTime<Utc>,
}

/// Turns snapshots of ESB's list into the events worth sending.
#[derive(Debug)]
pub struct OutagePublisher {
    scope: Scope,
    stale: Duration,
    refresh: chrono::Duration,
    held: HashMap<String, Held>,
    outbox: Vec<Event>,
    counters: FeedCounters,
}

impl OutagePublisher {
    /// A publisher whose markers live for `stale` and are republished every
    /// `refresh`, which is held to half of `stale` so a marker never blinks.
    #[must_use]
    pub fn new(scope: Scope, stale: Duration, refresh: Duration) -> Self {
        let ceiling = stale / 2;

        if refresh > ceiling {
            info!(
                refresh_s = ceiling.as_secs(),
                "`refresh` is more than half of `stale`, which would let markers expire between \
                 publications; using {}s.",
                ceiling.as_secs(),
            );
        }

        Self {
            scope,
            stale,
            refresh: chrono::Duration::from_std(refresh.min(ceiling))
                .unwrap_or_else(|_| chrono::Duration::zero()),
            held: HashMap::new(),
            outbox: Vec::new(),
            counters: FeedCounters::default(),
        }
    }

    /// Offers everything ESB currently lists.
    pub fn offer(&mut self, snapshot: Vec<Outage>) {
        self.offer_at(snapshot, Utc::now());
    }

    /// [`offer`](Self::offer), at an instant of the caller's choosing.
    pub fn offer_at(&mut self, snapshot: Vec<Outage>, now: DateTime<Utc>) {
        let mut listed = HashSet::with_capacity(snapshot.len());

        for outage in snapshot {
            self.counters.offered += 1;

            if !self.scope.admits(&outage) {
                self.counters.suppressed += 1;
                continue;
            }

            listed.insert(outage.id.clone());

            let current = self
                .held
                .get(&outage.id)
                .is_some_and(|held| held.outage == outage && now < held.due);

            if current {
                self.counters.suppressed += 1;
                continue;
            }

            self.outbox.push(outage.to_event(now, self.stale));
            self.counters.published += 1;

            let due = now + self.refresh_after(&outage.id);
            self.held.insert(outage.id.clone(), Held { outage, due });
        }

        let before = self.held.len();
        self.held.retain(|id, _| listed.contains(id));
        self.counters.expired += (before - self.held.len()) as u64;
    }

    /// Between half of `refresh` and all of it, fixed per outage.
    ///
    /// Everything a storm lists arrives in one poll; without a spread it would
    /// all be republished in one burst every `refresh` for the rest of the day.
    fn refresh_after(&self, id: &str) -> chrono::Duration {
        let spread: i32 = id.bytes().map(i32::from).sum::<i32>() % 50;

        self.refresh * (50 + spread) / 100
    }

    /// Takes what is waiting to be sent.
    pub fn drain(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.outbox)
    }

    /// Makes every marker due, for a connection that has none of them.
    pub fn refresh_all(&mut self) {
        for held in self.held.values_mut() {
            held.due = DateTime::<Utc>::MIN_UTC;
        }
    }

    /// What was offered, published, suppressed and dropped.
    #[must_use]
    pub const fn counters(&self) -> FeedCounters {
        self.counters
    }

    /// What is on the map.
    #[must_use]
    pub fn summary(&self) -> Summary {
        let mut summary = Summary::default();

        for held in self.held.values() {
            match held.outage.kind {
                OutageKind::Fault => {
                    summary.fault += 1;
                    summary.customers += held.outage.customers.unwrap_or_default();
                }
                OutageKind::Planned => summary.planned += 1,
                OutageKind::Restored => summary.restored += 1,
                OutageKind::Other => summary.other += 1,
            }
        }

        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::feed::Area;

    const STALE: Duration = Duration::from_secs(900);
    const REFRESH: Duration = Duration::from_secs(300);

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    fn publisher() -> OutagePublisher {
        OutagePublisher::new(Scope::default(), STALE, REFRESH)
    }

    fn cork() -> Outage {
        Outage::new("1", OutageKind::Fault, (51.8139, -8.3986))
    }

    fn galway() -> Outage {
        Outage::new("2", OutageKind::Planned, (53.2719, -9.0489))
    }

    #[test]
    fn an_outage_is_published_when_it_appears_and_not_again_while_nothing_changes() {
        let mut publisher = publisher();

        publisher.offer_at(vec![cork(), galway()], at(0));
        assert_eq!(publisher.drain().len(), 2);

        publisher.offer_at(vec![cork(), galway()], at(10));
        assert!(publisher.drain().is_empty());
        assert_eq!(publisher.counters().suppressed, 2);
    }

    #[test]
    fn anything_esb_changes_is_published_at_once() {
        let mut publisher = publisher();
        publisher.offer_at(vec![cork()], at(0));
        let _ = publisher.drain();

        let updated = Outage {
            customers: Some(412),
            ..cork()
        };
        publisher.offer_at(vec![updated], at(10));

        let events = publisher.drain();
        assert_eq!(events.len(), 1);
        assert!(
            events[0]
                .callsign()
                .is_some_and(|label| label.contains("412"))
        );
    }

    #[test]
    fn an_unchanged_outage_is_republished_before_its_marker_goes_stale() {
        let mut publisher = publisher();
        publisher.offer_at(vec![cork()], at(0));
        let _ = publisher.drain();

        publisher.offer_at(vec![cork()], at(REFRESH.as_secs() as i64));

        assert_eq!(publisher.drain().len(), 1);
    }

    #[test]
    fn an_outage_esb_stops_listing_is_dropped_without_a_delete() {
        let mut publisher = publisher();
        publisher.offer_at(vec![cork(), galway()], at(0));
        let _ = publisher.drain();

        publisher.offer_at(vec![galway()], at(10));

        assert!(publisher.drain().is_empty(), "the marker expires by itself");
        assert_eq!(publisher.summary().total(), 1);
        assert_eq!(publisher.counters().expired, 1);
    }

    #[test]
    fn what_is_outside_the_scope_never_goes_out() {
        let munster = Area::Circle {
            lat: 51.9,
            lon: -8.47,
            radius_km: 50.0,
        };
        let scope = Scope::new(munster, vec![OutageKind::Fault]);
        let mut publisher = OutagePublisher::new(scope, STALE, REFRESH);
        let planned_in_cork = Outage::new("3", OutageKind::Planned, (51.9, -8.47));

        publisher.offer_at(vec![cork(), galway(), planned_in_cork], at(0));

        let events = publisher.drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid, "ESB-1");
    }

    #[test]
    fn a_reconnection_makes_every_marker_due_again() {
        let mut publisher = publisher();
        publisher.offer_at(vec![cork(), galway()], at(0));
        let _ = publisher.drain();

        publisher.refresh_all();
        publisher.offer_at(vec![cork(), galway()], at(10));

        assert_eq!(publisher.drain().len(), 2);
    }

    #[test]
    fn a_refresh_longer_than_half_of_stale_is_shortened_so_markers_never_blink() {
        let mut publisher = OutagePublisher::new(Scope::default(), STALE, STALE);
        publisher.offer_at(vec![cork()], at(0));
        let _ = publisher.drain();

        publisher.offer_at(vec![cork()], at(STALE.as_secs() as i64 / 2));

        assert_eq!(publisher.drain().len(), 1);
    }

    #[test]
    fn the_summary_counts_customers_off_supply_across_the_faults() {
        let mut publisher = publisher();
        let fault = Outage {
            customers: Some(412),
            ..cork()
        };
        let planned = Outage {
            customers: Some(120),
            ..galway()
        };

        publisher.offer_at(vec![fault, planned], at(0));

        let summary = publisher.summary();
        assert_eq!((summary.fault, summary.planned), (1, 1));
        assert_eq!(
            summary.customers, 412,
            "planned works are not people off supply yet"
        );
    }
}
