//! Who may talk to whom: group membership as two bit vectors.
//!
//! A TAK "channel" is a group, and a membership in one carries a direction: `IN`
//! means the holder may **publish** into the group, `OUT` means they may
//! **receive** from it. The two are independent, which is what lets a watch
//! officer subscribe to a channel they never post in, and a sensor feed post
//! into one it never reads.
//!
//! Routing asks one question, for every message, against every connected EUD:
//! *may this sender reach this receiver?* The answer is
//!
//! ```text
//! can_reach(sender, receiver) = sender.in ∩ receiver.out ≠ ∅
//! ```
//!
//! — the sender must be allowed to publish into some group the receiver is
//! allowed to receive from. With a few hundred EUDs on a stream listener that
//! question is asked tens of thousands of times a second, so the sets are bit
//! vectors and the question is a word-wise AND: each group owns a *bit position*
//! allocated once when it is created, and a [`GroupSet`] is two fixed-width bit
//! vectors over those positions.
//!
//! # Fixed width, and why
//!
//! [`GROUP_BITS`] positions, fixed. A fixed width means [`to_bytes`] is a fixed
//! 64-byte BLOB, comparisons need no length checks, and the routing hot path
//! never allocates. 256 groups is far beyond what a TAK deployment uses (TAK
//! Server's own channel model is the same order), and a deployment that
//! genuinely outgrew it would need a schema change either way.
//!
//! Bit 0 is not used, so that a zeroed row is never a valid membership; bit 1 is
//! `__ANON__`, created by the first migration.
//!
//! [`to_bytes`]: GroupSet::to_bytes

use std::collections::HashMap;

use bitvec::prelude::{BitVec, Msb0, bitvec};

use rustak_api::identity::{Direction, GroupName};

/// How many group bit positions a [`GroupSet`] holds.
pub const GROUP_BITS: usize = 256;

/// The bit position reserved for `__ANON__`, allocated by the first migration.
pub const ANON_BITPOS: u32 = 1;

/// The stored size of a [`GroupSet`]: [`GROUP_BITS`] of `in` then of `out`.
pub const GROUP_SET_BYTES: usize = GROUP_BITS / 8 * 2;

/// One principal's group rights, as two fixed-width bit vectors.
///
/// ```
/// # use rustak_core::identity::{Direction, GroupSet, can_reach};
/// let mut sensor = GroupSet::new();
/// sensor.set(7, Direction::In); // publishes into group 7, reads nothing
///
/// let mut watch = GroupSet::new();
/// watch.set(7, Direction::Out); // reads group 7, publishes nothing
///
/// assert!(can_reach(&sensor, &watch));
/// assert!(!can_reach(&watch, &sensor));
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct GroupSet {
    /// Groups this principal may publish into.
    bits_in: BitVec<u8, Msb0>,
    /// Groups this principal may receive from.
    bits_out: BitVec<u8, Msb0>,
}

impl GroupSet {
    /// A principal with no group rights at all — the safe starting point.
    pub fn new() -> Self {
        Self {
            bits_in: bitvec![u8, Msb0; 0; GROUP_BITS],
            bits_out: bitvec![u8, Msb0; 0; GROUP_BITS],
        }
    }

    /// Grants `direction` on the group at `bitpos`.
    ///
    /// A position outside [`GROUP_BITS`] is ignored rather than panicking: the
    /// value comes from a database row, and a row that outran the width is a
    /// migration problem, not a reason to drop every connected EUD.
    pub fn set(&mut self, bitpos: u32, direction: Direction) {
        let Some(index) = Self::index(bitpos) else {
            tracing::warn!(
                bitpos,
                "Ignoring a group bit position outside the set width."
            );
            return;
        };

        if direction.includes(Direction::In) {
            self.bits_in.set(index, true);
        }
        if direction.includes(Direction::Out) {
            self.bits_out.set(index, true);
        }
    }

    /// Revokes `direction` on the group at `bitpos`.
    pub fn clear(&mut self, bitpos: u32, direction: Direction) {
        let Some(index) = Self::index(bitpos) else {
            return;
        };

        if direction.includes(Direction::In) {
            self.bits_in.set(index, false);
        }
        if direction.includes(Direction::Out) {
            self.bits_out.set(index, false);
        }
    }

    /// Whether this set grants every right `direction` names at `bitpos`.
    pub fn contains(&self, bitpos: u32, direction: Direction) -> bool {
        let Some(index) = Self::index(bitpos) else {
            return false;
        };

        let granted_in = !direction.includes(Direction::In) || self.bits_in[index];
        let granted_out = !direction.includes(Direction::Out) || self.bits_out[index];

        granted_in && granted_out
    }

    /// Whether this principal has no group rights at all, and so can neither
    /// reach nor be reached by anybody.
    pub fn is_empty(&self) -> bool {
        self.bits_in.not_any() && self.bits_out.not_any()
    }

    /// The bit positions granted in one direction, ascending.
    pub fn positions(&self, direction: Direction) -> Vec<u32> {
        let bits = match direction {
            Direction::In | Direction::Both => &self.bits_in,
            Direction::Out => &self.bits_out,
        };

        bits.iter_ones()
            .filter(|index| direction != Direction::Both || self.bits_out[*index])
            .map(|index| index as u32)
            .collect()
    }

    /// Restricts this set to the groups `other` also grants, in each direction.
    ///
    /// This is how a subscription's effective rights are computed: a user's
    /// memberships intersected with the per-device active state that
    /// `PUT /Marti/api/groups/active` maintains.
    pub fn intersect(&mut self, other: &Self) {
        for index in 0..GROUP_BITS {
            let keep_in = self.bits_in[index] && other.bits_in[index];
            self.bits_in.set(index, keep_in);

            let keep_out = self.bits_out[index] && other.bits_out[index];
            self.bits_out.set(index, keep_out);
        }
    }

    /// The stored form: [`GROUP_SET_BYTES`] bytes, `in` then `out`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.bits_in.clone().into_vec();
        bytes.extend_from_slice(&self.bits_out.clone().into_vec());
        bytes
    }

    /// Reads the stored form back.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::System`] error when the BLOB is not
    /// [`GROUP_SET_BYTES`] long — a row written by a different schema, which we
    /// must not silently reinterpret as a *different set of rights*.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, human_errors::Error> {
        if bytes.len() != GROUP_SET_BYTES {
            return Err(human_errors::system(
                format!(
                    "A stored group set was {} bytes, but {GROUP_SET_BYTES} were expected.",
                    bytes.len()
                ),
                crate::errors::ADVICE_REPORT_DEV,
            ));
        }

        let (stored_in, stored_out) = bytes.split_at(GROUP_BITS / 8);

        Ok(Self {
            bits_in: BitVec::from_slice(stored_in),
            bits_out: BitVec::from_slice(stored_out),
        })
    }

    /// The names of the groups granted in one direction, for the admin API and
    /// `GET /Marti/api/groups/all`.
    pub fn names(&self, index: &GroupIndex, direction: Direction) -> Vec<GroupName> {
        self.positions(direction)
            .into_iter()
            .filter_map(|bitpos| index.name(bitpos).cloned())
            .collect()
    }

    /// Maps a bit position onto an index, rejecting the reserved zero position
    /// and anything past the width.
    fn index(bitpos: u32) -> Option<usize> {
        let index = bitpos as usize;
        (index != 0 && index < GROUP_BITS).then_some(index)
    }
}

impl Default for GroupSet {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GroupSet {
    /// Prints the granted positions rather than 512 bits of mostly zeroes.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GroupSet")
            .field("in", &self.positions(Direction::In))
            .field("out", &self.positions(Direction::Out))
            .finish()
    }
}

/// The routing question, asked for every message against every subscriber.
///
/// True when the sender may publish into a group the receiver may receive from.
pub fn can_reach(sender: &GroupSet, receiver: &GroupSet) -> bool {
    sender
        .bits_in
        .iter_ones()
        .any(|index| receiver.bits_out[index])
}

/// The bit position ↔ name mapping, loaded once from the `groups` table.
///
/// Bit positions are what routing uses and names are what people and TAK clients
/// use; this is the one place the two are related, so that neither side has to
/// carry the other around.
#[derive(Clone, Debug, Default)]
pub struct GroupIndex {
    by_bitpos: HashMap<u32, GroupName>,
    by_name: HashMap<GroupName, u32>,
}

impl GroupIndex {
    /// An empty index, before the `groups` table has been read.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a group's position and name.
    pub fn insert(&mut self, bitpos: u32, name: GroupName) {
        self.by_name.insert(name.clone(), bitpos);
        self.by_bitpos.insert(bitpos, name);
    }

    /// The name of the group at `bitpos`, if we know of one.
    pub fn name(&self, bitpos: u32) -> Option<&GroupName> {
        self.by_bitpos.get(&bitpos)
    }

    /// The position of a named group, if it exists.
    pub fn bitpos(&self, name: &GroupName) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// How many groups are known.
    pub fn len(&self) -> usize {
        self.by_bitpos.len()
    }

    /// Whether no groups are known yet.
    pub fn is_empty(&self) -> bool {
        self.by_bitpos.is_empty()
    }
}

impl FromIterator<(u32, GroupName)> for GroupIndex {
    fn from_iter<I: IntoIterator<Item = (u32, GroupName)>>(entries: I) -> Self {
        let mut index = Self::new();
        for (bitpos, name) in entries {
            index.insert(bitpos, name);
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set granting the listed positions in the listed direction.
    fn set_of(grants: &[(u32, Direction)]) -> GroupSet {
        let mut set = GroupSet::new();
        for (bitpos, direction) in grants {
            set.set(*bitpos, *direction);
        }
        set
    }

    #[test]
    fn reachability_is_the_senders_in_against_the_receivers_out() {
        // The rule the whole module exists to answer, stated as the design
        // states it, in the asymmetric case that makes it non-obvious.
        let sensor = set_of(&[(7, Direction::In)]);
        let watch = set_of(&[(7, Direction::Out)]);

        assert!(can_reach(&sensor, &watch));
        assert!(!can_reach(&watch, &sensor));
    }

    #[test]
    fn sharing_a_group_in_the_same_direction_is_not_reachability() {
        // Two read-only observers on one channel must not be able to send to
        // each other, which a naive "do the sets overlap" test would allow.
        let one = set_of(&[(7, Direction::Out)]);
        let other = set_of(&[(7, Direction::Out)]);

        assert!(!can_reach(&one, &other));
    }

    #[test]
    fn a_principal_with_no_groups_can_neither_reach_nor_be_reached() {
        // The starting point has to be deny, not allow: a user whose
        // memberships failed to load must not be handed the whole server.
        let nobody = GroupSet::new();
        let somebody = set_of(&[(7, Direction::Both)]);

        assert!(nobody.is_empty());
        assert!(!can_reach(&nobody, &somebody));
        assert!(!can_reach(&somebody, &nobody));
    }

    #[test]
    fn both_grants_each_direction_separately() {
        let set = set_of(&[(7, Direction::Both)]);

        assert!(set.contains(7, Direction::In));
        assert!(set.contains(7, Direction::Out));
        assert!(set.contains(7, Direction::Both));
    }

    #[test]
    fn one_direction_does_not_imply_the_other() {
        let set = set_of(&[(7, Direction::In)]);

        assert!(set.contains(7, Direction::In));
        assert!(!set.contains(7, Direction::Out));
        assert!(!set.contains(7, Direction::Both));
    }

    #[test]
    fn revoking_one_direction_leaves_the_other_in_place() {
        let mut set = set_of(&[(7, Direction::Both)]);
        set.clear(7, Direction::In);

        assert!(!set.contains(7, Direction::In));
        assert!(set.contains(7, Direction::Out));
    }

    #[test]
    fn bit_zero_is_never_a_membership() {
        // A zeroed row must not read as a valid grant, which is why position 0
        // is reserved and `__ANON__` starts at 1.
        let mut set = GroupSet::new();
        set.set(0, Direction::Both);

        assert!(!set.contains(0, Direction::Both));
        assert!(set.is_empty());
    }

    #[test]
    fn a_position_past_the_width_is_ignored_rather_than_fatal() {
        // The value comes from a database row; a row that outran the width is a
        // migration problem, and dropping every connected EUD over it would be
        // a far worse outage than one group not routing.
        let mut set = GroupSet::new();
        set.set(GROUP_BITS as u32 + 1, Direction::Both);

        assert!(set.is_empty());
    }

    #[test]
    fn a_set_survives_the_trip_through_a_database_blob() {
        let original = set_of(&[
            (ANON_BITPOS, Direction::Both),
            (7, Direction::In),
            (200, Direction::Out),
        ]);

        let bytes = original.to_bytes();
        assert_eq!(bytes.len(), GROUP_SET_BYTES);

        let read = GroupSet::from_bytes(&bytes).unwrap();
        assert_eq!(read, original);
        assert!(read.contains(7, Direction::In) && !read.contains(7, Direction::Out));
        assert!(read.contains(200, Direction::Out) && !read.contains(200, Direction::In));
    }

    #[test]
    fn the_two_directions_do_not_bleed_into_one_another_in_storage() {
        // `in` and `out` are concatenated, so an off-by-one in the split would
        // silently hand somebody the other half of their own rights.
        let bytes = set_of(&[(7, Direction::In)]).to_bytes();
        let read = GroupSet::from_bytes(&bytes).unwrap();

        assert_eq!(read.positions(Direction::In), vec![7]);
        assert!(read.positions(Direction::Out).is_empty());
    }

    #[test]
    fn a_blob_from_a_different_schema_is_refused_rather_than_reinterpreted() {
        // Reading a short row as a set would not fail loudly — it would grant a
        // *different set of rights*, which is the worst possible failure here.
        assert!(GroupSet::from_bytes(&[0u8; 8]).is_err());
        assert!(GroupSet::from_bytes(&[]).is_err());
    }

    #[test]
    fn intersecting_is_how_a_devices_active_groups_narrow_a_users_memberships() {
        // `PUT /Marti/api/groups/active` turns groups off for one device
        // without touching the user's membership, so the effective set is the
        // intersection of the two.
        let mut effective = set_of(&[(7, Direction::Both), (8, Direction::Both)]);
        let active = set_of(&[(7, Direction::Both)]);

        effective.intersect(&active);

        assert!(effective.contains(7, Direction::Both));
        assert!(!effective.contains(8, Direction::In));
        assert!(!effective.contains(8, Direction::Out));
    }

    #[test]
    fn positions_are_reported_in_order_so_the_admin_api_is_stable() {
        let set = set_of(&[(200, Direction::In), (7, Direction::In), (1, Direction::In)]);

        assert_eq!(set.positions(Direction::In), vec![1, 7, 200]);
    }

    #[test]
    fn a_set_names_its_groups_through_the_index() {
        let index: GroupIndex = [
            (ANON_BITPOS, GroupName::anon()),
            (7, GroupName::parse("Blue Team").unwrap()),
        ]
        .into_iter()
        .collect();

        let set = set_of(&[(ANON_BITPOS, Direction::Both), (7, Direction::Out)]);

        assert_eq!(
            set.names(&index, Direction::Out),
            vec![GroupName::anon(), GroupName::parse("Blue Team").unwrap()],
        );
        assert_eq!(index.bitpos(&GroupName::anon()), Some(ANON_BITPOS));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn a_group_we_have_no_name_for_is_left_out_rather_than_guessed_at() {
        // A membership whose group row was deleted still routes by position;
        // inventing a name for the admin UI would be a lie.
        let index: GroupIndex = [(7, GroupName::parse("Blue Team").unwrap())]
            .into_iter()
            .collect();
        let set = set_of(&[(7, Direction::Out), (9, Direction::Out)]);

        assert_eq!(set.names(&index, Direction::Out).len(), 1);
        assert!(index.name(9).is_none());
    }

    #[test]
    fn debugging_a_set_prints_its_groups_rather_than_five_hundred_bits() {
        let set = set_of(&[(7, Direction::In), (8, Direction::Out)]);

        let printed = format!("{set:?}");
        assert!(printed.contains("in: [7]"), "{printed}");
        assert!(printed.contains("out: [8]"), "{printed}");
    }
}
