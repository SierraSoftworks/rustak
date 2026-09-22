//! What is on the map, as the page remembers it.
//!
//! The server sends features; this decides which of them are news. A uid is
//! replaced only by a copy with a later event time, because the snapshot and
//! the feed overlap on purpose and arrive in either order. A feature that has
//! gone stale is kept — dimmed — for a few minutes and then dropped, so that
//! "it stopped reporting" is something to look at rather than an absence.
//!
//! Every method answers a [`Changes`]: what to draw again and what to take
//! away. The clock is always passed in, so none of this needs a browser.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use rustak_api::{MapFeature, Symbology};
use serde_json::Value;

use super::render;

/// How long a stale feature stays, dimmed. The server's snapshot uses the same
/// figure, so a reload does not bring back what the page had already dropped.
const LINGER: Duration = Duration::minutes(5);

/// How recently something must have arrived to survive a snapshot that does
/// not mention it — the snapshot was read a moment before it was relayed.
const JUST_ARRIVED: Duration = Duration::seconds(10);

/// What a change to the store means for the map.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub upserts: Vec<Value>,
    pub removes: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.removes.is_empty()
    }
}

struct Held {
    feature: MapFeature,
    /// Whether it was stale the last time it was drawn, so that the moment it
    /// becomes so is a redraw and every moment after is not.
    drawn_stale: bool,
}

#[derive(Default)]
pub struct Store {
    held: HashMap<String, Held>,
    /// The edition of MIL-STD-2525 everything here was last drawn in.
    symbology: Symbology,
}

impl Store {
    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn get(&self, uid: &str) -> Option<&MapFeature> {
        self.held.get(uid).map(|held| &held.feature)
    }

    /// Everything matching `search`, by callsign. The search is of callsigns,
    /// uids and CoT types, without regard to case.
    pub fn roster(&self, search: &str) -> Vec<&MapFeature> {
        let wanted = search.trim().to_lowercase();
        let name = |feature: &MapFeature| {
            feature
                .callsign
                .clone()
                .unwrap_or_else(|| feature.uid.clone())
                .to_lowercase()
        };

        let mut listed: Vec<&MapFeature> = self
            .held
            .values()
            .map(|held| &held.feature)
            .filter(|feature| {
                wanted.is_empty()
                    || name(feature).contains(&wanted)
                    || feature.uid.to_lowercase().contains(&wanted)
                    || feature.kind.to_lowercase().starts_with(&wanted)
            })
            .collect();

        listed.sort_by_cached_key(|feature| (name(feature), feature.uid.clone()));
        listed
    }

    /// Takes one feature off the feed.
    pub fn upsert(&mut self, feature: MapFeature, now: DateTime<Utc>) -> Changes {
        let mut changes = Changes::default();

        if feature.stale + LINGER < now {
            return changes;
        }
        if self
            .get(&feature.uid)
            .is_some_and(|known| known.time > feature.time)
        {
            return changes;
        }

        changes
            .upserts
            .push(render::draw(&feature, self.symbology, now));
        self.held.insert(
            feature.uid.clone(),
            Held {
                drawn_stale: feature.stale < now,
                feature,
            },
        );

        changes
    }

    pub fn remove(&mut self, uid: &str) -> Changes {
        Changes {
            upserts: Vec::new(),
            removes: self
                .held
                .remove(uid)
                .map(|_| uid.to_string())
                .into_iter()
                .collect(),
        }
    }

    /// Draws everything again in another edition of MIL-STD-2525. Nothing to
    /// do, and nothing answered, when it is the edition already in use.
    pub fn restyle(&mut self, symbology: Symbology, now: DateTime<Utc>) -> Changes {
        if symbology == self.symbology {
            return Changes::default();
        }
        self.symbology = symbology;

        Changes {
            upserts: self
                .held
                .values()
                .map(|held| render::draw(&held.feature, symbology, now))
                .collect(),
            removes: Vec::new(),
        }
    }

    /// Forgets everything, for a reader who may no longer see any of it.
    pub fn clear(&mut self) -> Changes {
        Changes {
            upserts: Vec::new(),
            removes: self.held.drain().map(|(uid, _)| uid).collect(),
        }
    }

    /// Takes a snapshot: everything in it, and nothing that is not — except
    /// what arrived so recently that the snapshot could not have known.
    pub fn replace(&mut self, snapshot: Vec<MapFeature>, now: DateTime<Utc>) -> Changes {
        let mut changes = Changes::default();

        // A set, because both sides can be thousands long and this runs on the
        // page's only thread.
        let listed: HashSet<&str> = snapshot
            .iter()
            .map(|feature| feature.uid.as_str())
            .collect();

        let gone: Vec<String> = self
            .held
            .values()
            .map(|held| &held.feature)
            .filter(|known| known.received_at + JUST_ARRIVED < now)
            .filter(|known| !listed.contains(known.uid.as_str()))
            .map(|known| known.uid.clone())
            .collect();

        for uid in gone {
            changes.removes.extend(self.remove(&uid).removes);
        }
        for feature in snapshot {
            changes.upserts.extend(self.upsert(feature, now).upserts);
        }

        changes
    }

    /// Lets time pass: dims what has gone stale, drops what has been stale long
    /// enough.
    pub fn sweep(&mut self, now: DateTime<Utc>) -> Changes {
        let mut changes = Changes::default();

        self.held.retain(|uid, held| {
            let keep = held.feature.stale + LINGER >= now;
            if !keep {
                changes.removes.push(uid.clone());
            }
            keep
        });

        for held in self.held.values_mut() {
            let stale = held.feature.stale < now;
            if stale != held.drawn_stale {
                held.drawn_stale = stale;
                changes
                    .upserts
                    .push(render::draw(&held.feature, self.symbology, now));
            }
        }

        changes
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::MapPoint;

    use super::*;

    fn at(time: &str) -> DateTime<Utc> {
        format!("2026-09-18T{time}Z").parse().unwrap()
    }

    /// Sent at `time`, stale two minutes later.
    fn feature(uid: &str, time: &str) -> MapFeature {
        MapFeature {
            uid: uid.to_string(),
            kind: "a-f-G-U-C".to_string(),
            how: None,
            callsign: Some(uid.to_lowercase()),
            team: None,
            role: None,
            time: at(time),
            stale: at(time) + Duration::minutes(2),
            received_at: at(time),
            point: MapPoint {
                lat: 51.5,
                lon: -0.12,
                hae: None,
                ce: None,
                le: None,
            },
            shape: None,
            course: None,
            speed: None,
            battery: None,
            remarks: None,
            software: None,
            groups: Vec::new(),
        }
    }

    #[test]
    fn an_older_copy_never_replaces_a_newer_one() {
        let mut store = Store::default();

        assert_eq!(
            store
                .upsert(feature("A", "12:00:30"), at("12:00:31"))
                .upserts
                .len(),
            1
        );
        assert!(
            store
                .upsert(feature("A", "12:00:00"), at("12:00:32"))
                .is_empty()
        );
        assert_eq!(store.get("A").unwrap().time, at("12:00:30"));
    }

    #[test]
    fn a_snapshot_removes_what_it_does_not_mention_unless_it_only_just_arrived() {
        let mut store = Store::default();
        store.upsert(feature("OLD", "12:00:00"), at("12:00:00"));
        store.upsert(feature("NEW", "12:00:58"), at("12:00:58"));

        let changes = store.replace(vec![feature("LISTED", "12:00:50")], at("12:01:00"));

        assert_eq!(changes.removes, ["OLD"]);
        assert_eq!(changes.upserts.len(), 1);
        assert!(store.get("NEW").is_some());
    }

    #[test]
    fn going_stale_is_one_redraw_and_lingering_ends_in_a_removal() {
        let mut store = Store::default();
        store.upsert(feature("A", "12:00:00"), at("12:00:00"));

        assert!(store.sweep(at("12:01:00")).is_empty());

        let dimmed = store.sweep(at("12:02:30"));
        assert_eq!(dimmed.upserts[0]["anchor"]["properties"]["stale"], true);
        assert!(store.sweep(at("12:03:00")).is_empty());

        assert_eq!(store.sweep(at("12:07:30")).removes, ["A"]);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn choosing_another_edition_redraws_what_is_held_and_what_arrives_after() {
        let mut store = Store::default();
        store.upsert(feature("A", "12:00:00"), at("12:00:00"));

        assert!(
            store
                .restyle(Symbology::Milstd2525C, at("12:00:01"))
                .is_empty()
        );

        let redrawn = store.restyle(Symbology::Milstd2525D, at("12:00:01"));
        assert_eq!(
            redrawn.upserts[0]["anchor"]["properties"]["icon"],
            "sidc:2525d:SFGPUC---------"
        );

        let later = store.upsert(feature("B", "12:00:02"), at("12:00:02"));
        assert_eq!(
            later.upserts[0]["anchor"]["properties"]["icon"],
            "sidc:2525d:SFGPUC---------"
        );
    }

    #[test]
    fn clearing_takes_everything_off_the_map() {
        let mut store = Store::default();
        store.upsert(feature("A", "12:00:00"), at("12:00:00"));

        assert_eq!(store.clear().removes, ["A"]);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn something_long_stale_is_never_taken_at_all() {
        let mut store = Store::default();

        assert!(
            store
                .upsert(feature("A", "11:00:00"), at("12:00:00"))
                .is_empty()
        );
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn the_roster_is_by_callsign_and_searches_uids_and_types_too() {
        let mut store = Store::default();
        for uid in ["BRAVO", "ALPHA", "CHARLIE"] {
            store.upsert(feature(uid, "12:00:00"), at("12:00:00"));
        }

        let names = |search: &str| -> Vec<String> {
            store.roster(search).iter().map(|f| f.uid.clone()).collect()
        };

        assert_eq!(names(""), ["ALPHA", "BRAVO", "CHARLIE"]);
        assert_eq!(names("rav"), ["BRAVO"]);
        assert_eq!(names("a-f").len(), 3);
        assert!(names("a-h").is_empty());
    }
}
