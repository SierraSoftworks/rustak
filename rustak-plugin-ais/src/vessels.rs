//! What this plugin remembers about each hull between messages.
//!
//! AIS splits a vessel across two broadcasts: a position every few seconds and
//! a static report — name, call sign, IMO number, ship type, dimensions — every
//! six minutes. Neither is useful alone, and they arrive in whichever order the
//! receiver heard them, so a source hands both to [`Vessels`] and takes back
//! the tracks that are now worth publishing:
//!
//! * a **position** becomes a track immediately, enriched with whatever static
//!   data has already arrived for that MMSI;
//! * a **static report** re-offers the last position of that MMSI, so a hull
//!   labelled `MMSI 244660000` becomes `ZEEBRUGGE` without waiting for it to
//!   move. The publisher's `min_interval` decides whether that actually goes
//!   out, which is the right place for that decision.
//!
//! # Bounded, and forgetful in the right order
//!
//! Both halves are capped at the publisher's `max_tracks` and evicted
//! least-recently-seen, so a feed pointed at the whole world costs a known
//! amount of memory rather than an unknown one. Positions are forgotten on the
//! publisher's own expiry; static data is kept for [`STATIC_RETENTION`], which
//! is longer because it is broadcast far less often — a cache expiring between
//! two type 5 reports would rename every vessel `MMSI …` twice an hour.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustak_client::feed::Track;

use crate::mapping::{self, Position, StaticData};

/// How long a vessel's static data is kept after it was last heard.
///
/// Half an hour: type 5 is broadcast every six minutes under way and every six
/// minutes at anchor, so this survives several missed reports without holding a
/// name for a hull that has long since left the area.
pub const STATIC_RETENTION: Duration = Duration::from_secs(1_800);

/// One of the two things an AIS receiver hears.
#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    /// Where a vessel is.
    Position(Position),
    /// What a vessel says it is.
    Static(StaticData),
}

/// A value, and when it was last heard.
#[derive(Clone, Debug)]
struct Heard<T> {
    value: T,
    at: DateTime<Utc>,
}

/// The per-MMSI memory one source keeps.
#[derive(Debug)]
pub struct Vessels {
    /// What each vessel says it is.
    statics: HashMap<u32, Heard<StaticData>>,
    /// Where each vessel last was, so that late static data can re-offer it.
    positions: HashMap<u32, Heard<Position>>,
    /// The upstream's name, written into every track's `Source` remark.
    source: String,
    /// How long a position is kept — the publisher's own `stale`.
    retention: Duration,
    /// How many vessels of each half are held before the least recently heard
    /// is dropped.
    capacity: usize,
}

impl Vessels {
    /// A cache that forgets a position after `retention` and holds at most
    /// `capacity` vessels.
    #[must_use]
    pub fn new(source: impl Into<String>, retention: Duration, capacity: usize) -> Self {
        Self {
            statics: HashMap::new(),
            positions: HashMap::new(),
            source: source.into(),
            retention,
            capacity: capacity.max(1),
        }
    }

    /// Absorbs one observation, answering the track it makes publishable.
    pub fn absorb(&mut self, observation: Observation, now: DateTime<Utc>) -> Option<Track> {
        match observation {
            Observation::Position(position) => self.position(position, now),
            Observation::Static(statics) => self.statics(statics, now),
        }
    }

    /// Absorbs a batch, in order, answering every track it made publishable.
    pub fn absorb_all(
        &mut self,
        observations: impl IntoIterator<Item = Observation>,
    ) -> Vec<Track> {
        let now = Utc::now();
        let tracks: Vec<Track> = observations
            .into_iter()
            .filter_map(|observation| self.absorb(observation, now))
            .collect();

        self.expire(now);

        tracks
    }

    /// How many vessels have static data cached, for a status report.
    #[must_use]
    pub fn named(&self) -> usize {
        self.statics.len()
    }

    /// Forgets what has not been heard for long enough, and trims what is left
    /// to the capacity.
    pub fn expire(&mut self, now: DateTime<Utc>) {
        let retention = self.retention;
        Self::retain_newer_than(&mut self.positions, now, retention);
        Self::retain_newer_than(&mut self.statics, now, STATIC_RETENTION.max(retention));

        Self::trim(&mut self.positions, self.capacity);
        Self::trim(&mut self.statics, self.capacity);
    }

    /// A position: always a track, enriched with whatever we already know.
    fn position(&mut self, position: Position, now: DateTime<Utc>) -> Option<Track> {
        if !position.is_usable() {
            return None;
        }

        let track = mapping::track(
            &position,
            self.statics.get(&position.mmsi).map(|heard| &heard.value),
            &self.source,
        );

        self.positions.insert(
            position.mmsi,
            Heard {
                value: position,
                at: now,
            },
        );

        Some(track)
    }

    /// Static data: a track only if we have somewhere to put the hull.
    fn statics(&mut self, statics: StaticData, now: DateTime<Utc>) -> Option<Track> {
        let mmsi = statics.mmsi;
        self.statics.insert(
            mmsi,
            Heard {
                value: statics,
                at: now,
            },
        );

        // Re-offer the last position under the vessel's real name. The
        // observation time is the position's own, so the publisher sees the
        // same instant rather than a vessel that jumped back in time.
        let position = self.positions.get(&mmsi)?.value.clone();

        Some(mapping::track(
            &position,
            self.statics.get(&mmsi).map(|heard| &heard.value),
            &self.source,
        ))
    }

    /// Drops everything last heard longer ago than `retention`.
    fn retain_newer_than<T>(
        held: &mut HashMap<u32, Heard<T>>,
        now: DateTime<Utc>,
        retention: Duration,
    ) {
        held.retain(|_, heard| (now - heard.at).to_std().unwrap_or_default() <= retention);
    }

    /// Drops the least recently heard until the map fits.
    fn trim<T>(held: &mut HashMap<u32, Heard<T>>, capacity: usize) {
        while held.len() > capacity {
            let Some(oldest) = held
                .iter()
                .min_by_key(|(_, heard)| heard.at)
                .map(|(mmsi, _)| *mmsi)
            else {
                return;
            };

            held.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-09-20T12:00:00Z".parse().expect("an instant")
    }

    fn position(mmsi: u32) -> Position {
        Position {
            mmsi,
            position: (51.9512, 4.1338),
            sog_knots: Some(12.0),
            cog_deg: Some(271.5),
            heading_deg: Some(270.0),
            nav_status: Some(0),
            observed_at: now(),
        }
    }

    fn statics(mmsi: u32) -> StaticData {
        StaticData {
            mmsi,
            name: Some("ZEEBRUGGE".into()),
            ship_type: Some(70),
            ..StaticData::default()
        }
    }

    fn cache() -> Vessels {
        Vessels::new("receiver", Duration::from_secs(120), 100)
    }

    #[test]
    fn a_position_is_published_before_anyone_knows_the_vessels_name() {
        let mut vessels = cache();

        let track = vessels
            .absorb(Observation::Position(position(1)), now())
            .expect("a position is always a track");

        assert_eq!(track.callsign.as_deref(), Some("MMSI 1"));
    }

    #[test]
    fn static_data_arriving_later_re_offers_the_hull_under_its_name() {
        let mut vessels = cache();
        let _ = vessels.absorb(Observation::Position(position(1)), now());

        let named = vessels
            .absorb(Observation::Static(statics(1)), now())
            .expect("the last position is re-offered");

        assert_eq!(named.callsign.as_deref(), Some("ZEEBRUGGE"));
        assert_eq!(named.position, (51.9512, 4.1338));
        assert_eq!(
            named.observed_at,
            now(),
            "the position's instant, not this one: a vessel must not move backwards",
        );
        assert_eq!(vessels.named(), 1);
    }

    #[test]
    fn static_data_for_a_vessel_nobody_has_seen_is_cached_and_nothing_else() {
        let mut vessels = cache();

        assert!(
            vessels
                .absorb(Observation::Static(statics(7)), now())
                .is_none(),
            "there is no position to put a name on yet",
        );

        let track = vessels
            .absorb(Observation::Position(position(7)), now())
            .expect("the position that follows");

        assert_eq!(track.callsign.as_deref(), Some("ZEEBRUGGE"));
    }

    #[test]
    fn a_position_with_no_fix_is_dropped_rather_than_published() {
        let mut vessels = cache();
        let mut nowhere = position(3);
        nowhere.position = (91.0, 181.0);

        assert!(
            vessels
                .absorb(Observation::Position(nowhere), now())
                .is_none()
        );
    }

    #[test]
    fn a_position_is_forgotten_on_the_publishers_expiry_and_a_name_outlives_it() {
        let mut vessels = cache();
        let _ = vessels.absorb(Observation::Position(position(1)), now());
        let _ = vessels.absorb(Observation::Static(statics(1)), now());

        vessels.expire(now() + chrono::Duration::seconds(121));

        assert_eq!(vessels.positions.len(), 0, "past the publisher's stale");
        assert_eq!(vessels.named(), 1, "a type 5 report is six minutes apart");

        vessels.expire(now() + chrono::Duration::seconds(1_801));

        assert_eq!(vessels.named(), 0);
    }

    #[test]
    fn the_cache_is_bounded_and_drops_the_least_recently_heard() {
        let mut vessels = Vessels::new("receiver", Duration::from_secs(120), 2);

        for (nth, mmsi) in [10, 11, 12].into_iter().enumerate() {
            let at = now() + chrono::Duration::seconds(nth as i64);
            let _ = vessels.absorb(Observation::Position(position(mmsi)), at);
            vessels.expire(at);
        }

        assert_eq!(vessels.positions.len(), 2);
        assert!(
            !vessels.positions.contains_key(&10),
            "the oldest goes first"
        );
        assert!(vessels.positions.contains_key(&12));
    }

    #[test]
    fn a_batch_answers_one_track_per_publishable_observation() {
        let mut vessels = cache();

        let tracks = vessels.absorb_all([
            Observation::Position(position(1)),
            Observation::Position(position(2)),
            // Re-offers vessel 1, so three observations make three tracks.
            Observation::Static(statics(1)),
            // Nothing to re-offer: vessel 9 has never been seen.
            Observation::Static(statics(9)),
        ]);

        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[2].id, "AIS-1");
        assert_eq!(tracks[2].callsign.as_deref(), Some("ZEEBRUGGE"));
    }
}
