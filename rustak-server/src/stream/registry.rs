//! The map of live connections, and the two string indexes over it.
//!
//! Split out of [`Hub`](super::hub::Hub) because it is a different
//! responsibility: the hub answers routing questions and owns the lock, and
//! this owns the bookkeeping that makes those questions cheap.
//!
//! # Why an index entry is a list
//!
//! `<marti><dest callsign>` and `<dest uid>` are addressed by strings the
//! *client* chooses, and two clients can legitimately carry the same one — a
//! spare radio, a device handed over, a callsign somebody typed twice. A map to
//! a single id would silently make one of them unreachable, which is the kind
//! of fault that shows up as "chat sometimes does not arrive".

use std::collections::HashMap;

use super::subscription::{ConnHandle, ConnId, Subscription};

/// The live connections, and the indexes over them.
#[derive(Debug, Default)]
pub struct Registry {
    /// Every connection, by identifier.
    pub conns: HashMap<ConnId, Subscription>,
    /// The connections claiming each `clientUid`.
    pub by_uid: HashMap<String, Vec<ConnId>>,
    /// The connections reporting each callsign.
    pub by_callsign: HashMap<String, Vec<ConnId>>,
}

/// Which of the two string indexes an explicit address is looked up in.
#[derive(Clone, Copy, Debug)]
pub enum Index {
    /// `<dest uid>`.
    Uid,
    /// `<dest callsign>`.
    Callsign,
}

impl Index {
    /// The connection ids registered under `key`.
    pub fn lookup<'a>(self, registry: &'a Registry, key: &str) -> Option<&'a Vec<ConnId>> {
        match self {
            Self::Uid => registry.by_uid.get(key),
            Self::Callsign => registry.by_callsign.get(key),
        }
    }
}

impl Registry {
    /// Adds a subscription to the uid and callsign indexes.
    pub fn index(&mut self, subscription: &Subscription) {
        if let Some(uid) = &subscription.client_uid {
            self.by_uid
                .entry(uid.clone())
                .or_default()
                .push(subscription.id);
        }
        if let Some(callsign) = &subscription.callsign {
            self.by_callsign
                .entry(callsign.clone())
                .or_default()
                .push(subscription.id);
        }
    }

    /// Takes a subscription back out of both indexes, dropping an entry that
    /// has nothing left in it.
    pub fn unindex(&mut self, subscription: &Subscription) {
        if let Some(uid) = &subscription.client_uid {
            remove(&mut self.by_uid, uid, subscription.id);
        }
        if let Some(callsign) = &subscription.callsign {
            remove(&mut self.by_callsign, callsign, subscription.id);
        }
    }

    /// Moves one connection's entry in an index from `old` to `new`.
    ///
    /// Neither a callsign nor a `clientUid` is fixed for the life of a
    /// connection. Renaming a device mid-session is routine in ATAK, and every
    /// situational-awareness message after the rename carries the new name —
    /// but the index was only ever written when the *first* identifying message
    /// arrived. So `<dest callsign="BRAVO">` resolved to nobody while
    /// `<dest callsign="ALPHA">` still resolved to the right connection under
    /// the wrong name: a direct chat that silently vanishes, or bounces as
    /// `b-t-f-s` while the sender is looking at the person on the map.
    ///
    /// The stale entry was a leak as well as a fault, because
    /// [`unindex`](Self::unindex) only ever removes the name a subscription is
    /// *currently* carrying — one dead entry per rename, for the life of the
    /// process. R-03 M2.
    pub fn reindex(&mut self, index: Index, id: ConnId, old: Option<&str>, new: Option<&str>) {
        if old == new {
            return;
        }

        let map = match index {
            Index::Uid => &mut self.by_uid,
            Index::Callsign => &mut self.by_callsign,
        };

        if let Some(old) = old {
            remove(map, old, id);
        }

        if let Some(new) = new {
            let ids = map.entry(new.to_owned()).or_default();

            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }

    /// The handles for a list of ids, skipping any that have since gone.
    pub fn handles(&self, ids: &[ConnId]) -> Vec<ConnHandle> {
        ids.iter()
            .filter_map(|id| self.conns.get(id))
            .map(|subscription| subscription.handle.clone())
            .collect()
    }
}

/// Removes one id from an index entry, and the entry when it empties.
fn remove(index: &mut HashMap<String, Vec<ConnId>>, key: &str, id: ConnId) {
    let Some(ids) = index.get_mut(key) else {
        return;
    };

    ids.retain(|held| *held != id);

    if ids.is_empty() {
        index.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_entry_disappears_when_its_last_holder_does() {
        // A map that kept empty vectors for every callsign ever seen would be
        // a map that grows for the life of the process.
        let mut index: HashMap<String, Vec<ConnId>> = HashMap::new();
        index.insert("ALPHA".into(), vec![ConnId(1), ConnId(2)]);

        remove(&mut index, "ALPHA", ConnId(1));
        assert_eq!(index.get("ALPHA"), Some(&vec![ConnId(2)]));

        remove(&mut index, "ALPHA", ConnId(2));
        assert!(index.is_empty());
    }

    #[test]
    fn removing_something_that_was_never_there_is_not_a_failure() {
        let mut index: HashMap<String, Vec<ConnId>> = HashMap::new();

        remove(&mut index, "NOBODY", ConnId(1));

        assert!(index.is_empty());
    }

    #[test]
    fn each_index_looks_the_key_up_in_its_own_map() {
        let mut registry = Registry::default();
        registry.by_uid.insert("UID-A".into(), vec![ConnId(1)]);
        registry
            .by_callsign
            .insert("ALPHA".into(), vec![ConnId(1), ConnId(2)]);

        assert_eq!(
            Index::Uid.lookup(&registry, "UID-A"),
            Some(&vec![ConnId(1)])
        );
        assert!(Index::Uid.lookup(&registry, "ALPHA").is_none());
        assert_eq!(
            Index::Callsign.lookup(&registry, "ALPHA").map(Vec::len),
            Some(2),
            "two devices may share a callsign",
        );
    }
}
