//! The squashed change fold, held against a naive model.
//!
//! `GET …/changes?squashed=true` answers the current-state delta rather than
//! the history, and getting it wrong is invisible until a client's diff view
//! shows a file that is not there or hides one that is. The implementation
//! ([`rustak_server::missions::changes::squash`]) is a single reverse pass with
//! a `HashSet` of seen keys; the model below scans the whole history once per
//! key and picks the newest. They are written differently on purpose — two
//! implementations agreeing over generated histories is the evidence, and one
//! implementation agreeing with itself is not.
//!
//! The rule both sides implement:
//!
//! 1. Keep the newest row per `(type, item, creator)`.
//! 2. Keep an `ADD_CONTENT` only if the item is still filed.
//! 3. Keep a `REMOVE_CONTENT` only if it is not.
//! 4. Always keep the rows that are not about an item.
//!
//! Run with `cargo test -p rustak-server --features testing --test mission_squash`.

#![cfg(feature = "testing")]

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration};
use proptest::prelude::*;

use rustak_server::db::repos::MissionChangeRow;
use rustak_server::missions::changes::{Presence, squash};

/// The three kinds a generated history draws from.
const KINDS: &[&str] = &["ADD_CONTENT", "REMOVE_CONTENT", "CREATE_MISSION"];

/// The items a generated history files and unfiles.
const ITEMS: &[&str] = &["uid-a", "uid-b", "uid-c"];

/// The creators a generated history attributes changes to.
const CREATORS: &[&str] = &["ANDROID-1", "ANDROID-2"];

/// One generated change, before it is turned into a row.
#[derive(Debug, Clone)]
struct Step {
    kind: usize,
    item: usize,
    creator: usize,
}

/// Turns generated steps into rows, newest first, as the repository returns a
/// window.
fn history(steps: &[Step]) -> Vec<MissionChangeRow> {
    let base = DateTime::UNIX_EPOCH;
    let mut rows: Vec<MissionChangeRow> = steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let at = base + Duration::seconds(index as i64);
            let kind = KINDS[step.kind % KINDS.len()];
            let about_item = kind != "CREATE_MISSION";

            MissionChangeRow {
                id: index as i64 + 1,
                mission_id: 1,
                kind: kind.to_string(),
                timestamp: at,
                server_time: at,
                creator_uid: Some(CREATORS[step.creator % CREATORS.len()].to_string()),
                content_uid: about_item.then(|| ITEMS[step.item % ITEMS.len()].to_string()),
                content_hash: None,
                log_entry_id: None,
                map_layer_uid: None,
                feed_uid: None,
                is_federated: false,
                detail: None,
            }
        })
        .collect();

    rows.reverse();
    rows
}

/// What is filed after replaying a history in order.
///
/// The naive half of the property: presence is derived by replaying rather than
/// asserted, so the two sides cannot agree by sharing a mistake about it.
fn replay(rows: &[MissionChangeRow]) -> Presence {
    let mut uids: HashSet<String> = HashSet::new();

    // The rows arrive newest first; replaying means oldest first.
    for row in rows.iter().rev() {
        let Some(uid) = &row.content_uid else {
            continue;
        };

        match row.kind.as_str() {
            "ADD_CONTENT" => {
                uids.insert(uid.clone());
            }
            "REMOVE_CONTENT" => {
                uids.remove(uid);
            }
            _ => {}
        }
    }

    Presence {
        uids,
        hashes: HashSet::new(),
    }
}

/// The naive model: scan the whole history per key, then apply the two
/// presence rules.
fn model(rows: &[MissionChangeRow], present: &Presence) -> Vec<i64> {
    let mut newest: HashMap<(String, String, String), i64> = HashMap::new();

    for row in rows {
        let key = (
            row.kind.clone(),
            row.content_uid.clone().unwrap_or_default(),
            row.creator_uid.clone().unwrap_or_default(),
        );
        let slot = newest.entry(key).or_insert(row.id);

        if row.id > *slot {
            *slot = row.id;
        }
    }

    let mut kept: Vec<i64> = rows
        .iter()
        .filter(|row| {
            let key = (
                row.kind.clone(),
                row.content_uid.clone().unwrap_or_default(),
                row.creator_uid.clone().unwrap_or_default(),
            );

            newest.get(&key) == Some(&row.id)
        })
        .filter(|row| match (&row.content_uid, row.kind.as_str()) {
            (None, _) => true,
            (Some(uid), "ADD_CONTENT") => present.uids.contains(uid),
            (Some(uid), "REMOVE_CONTENT") => !present.uids.contains(uid),
            _ => true,
        })
        .map(|row| row.id)
        .collect();

    kept.sort_unstable();
    kept
}

/// A generated step.
fn step() -> impl Strategy<Value = Step> {
    (
        0usize..KINDS.len(),
        0usize..ITEMS.len(),
        0usize..CREATORS.len(),
    )
        .prop_map(|(kind, item, creator)| Step {
            kind,
            item,
            creator,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The fold and the model keep exactly the same rows.
    #[test]
    fn the_fold_agrees_with_a_naive_replay(steps in proptest::collection::vec(step(), 0..40)) {
        let rows = history(&steps);
        let present = replay(&rows);

        let mut folded: Vec<i64> = squash(rows.clone(), &present)
            .into_iter()
            .map(|row| row.id)
            .collect();
        folded.sort_unstable();

        prop_assert_eq!(folded, model(&rows, &present));
    }

    /// The squash never invents a row and never reorders the ones it keeps.
    #[test]
    fn the_fold_is_a_subsequence_of_its_input(steps in proptest::collection::vec(step(), 0..40)) {
        let rows = history(&steps);
        let present = replay(&rows);
        let folded = squash(rows.clone(), &present);

        let mut remaining = rows.iter().map(|row| row.id);

        for kept in &folded {
            prop_assert!(
                remaining.any(|id| id == kept.id),
                "the squash reordered or invented row {}",
                kept.id,
            );
        }
    }

    /// Every item still filed is reported by at least one surviving `ADD`, and
    /// no item that is filed is reported as removed.
    #[test]
    fn the_delta_describes_the_current_state(
        steps in proptest::collection::vec(step(), 1..40),
    ) {
        let rows = history(&steps);
        let present = replay(&rows);
        let folded = squash(rows.clone(), &present);

        for row in &folded {
            let Some(uid) = &row.content_uid else { continue };

            match row.kind.as_str() {
                "ADD_CONTENT" => prop_assert!(present.uids.contains(uid)),
                "REMOVE_CONTENT" => prop_assert!(!present.uids.contains(uid)),
                _ => {}
            }
        }

        for uid in &present.uids {
            prop_assert!(
                folded
                    .iter()
                    .any(|row| row.kind == "ADD_CONTENT" && row.content_uid.as_ref() == Some(uid)),
                "{uid} is filed and nothing in the delta says so",
            );
        }
    }
}

/// The canonical sequence the squash exists for, written out rather than
/// generated so that a failure names the case people talk about.
#[test]
fn add_remove_add_squashes_to_one_add() {
    let rows = history(&[
        Step {
            kind: 0,
            item: 0,
            creator: 0,
        },
        Step {
            kind: 1,
            item: 0,
            creator: 0,
        },
        Step {
            kind: 0,
            item: 0,
            creator: 0,
        },
    ]);
    let present = replay(&rows);
    let folded = squash(rows, &present);

    assert_eq!(folded.len(), 1, "{folded:?}");
    assert_eq!(folded[0].kind, "ADD_CONTENT");
    assert_eq!(folded[0].id, 3, "the newest one survives");
}
