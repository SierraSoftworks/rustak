//! What the sign-in rate limiter is refusing right now, and why.
//!
//! Every endpoint that accepts a secret counts its failures twice: against the
//! caller's address (tier 1, whatever it was guessing at) and against the
//! address and what it was guessing at (tier 2). Too many inside a window lock
//! that key out for a while. Before this existed, the only trace a lockout
//! left was the `429` its victim received, so an administrator asking "why was
//! I refused?" had nothing to look at (M9-14).
//!
//! # Estimates
//!
//! The limiter counts in a fixed-size sketch rather than a table of keys, so a
//! lockout's `failures` is an estimate — never below the real count, sometimes
//! above it when another key shares its cells — and the listing is a record of
//! the lockouts that began, kept in a bounded ring, checked against the sketch
//! when it is read. [`TierFill`] says how full each tier is, which is what the
//! chance of refusing somebody who never failed depends on.
//!
//! # Classes, not keys, are what gets counted
//!
//! A [`LockoutClass`] says what kind of thing the key names. The counters in
//! [`Lockouts`] are per class and never per key, so nothing that reads them
//! learns an address or a username. The list of current lockouts does carry
//! both, which is why it is for administrators only.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a limiter key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockoutClass {
    /// A sign-in endpoint on its own — passkeys, the setup token, a bearer
    /// exchange — so the address is all that tells one caller from another.
    Address,

    /// An account, by username: a password grant, a Basic credential, a
    /// workload identity once it has named the account it speaks for.
    Account,

    /// A confidential OAuth client, by client identifier.
    Client,

    /// Every sign-in from one address (IPv4 /32, IPv6 /64) or one IPv6 /48,
    /// whatever it was guessing at: tier 1.
    Source,
}

impl LockoutClass {
    /// Every class, in the order a reader is offered them.
    pub const ALL: [Self; 4] = [Self::Address, Self::Account, Self::Client, Self::Source];

    /// The value carried on the wire, and the only label a counter ever has.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Account => "account",
            Self::Client => "client",
            Self::Source => "source",
        }
    }

    /// A short phrase naming the class for somebody reading the console.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Address => "Address",
            Self::Account => "Account",
            Self::Client => "OAuth client",
            Self::Source => "Source",
        }
    }

    /// This class's position in [`ALL`](Self::ALL), for an array of counters.
    pub fn index(&self) -> usize {
        match self {
            Self::Address => 0,
            Self::Account => 1,
            Self::Client => 2,
            Self::Source => 3,
        }
    }
}

/// One key the limiter is refusing now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockout {
    pub class: LockoutClass,

    /// Where the failures came from, as the network address of the prefix
    /// they were counted at. Absent when the server could not tell, which in
    /// practice is a request that reached it over a Unix socket.
    #[serde(default)]
    pub address: Option<IpAddr>,

    /// The prefix length `address` was counted at: 32 for IPv4, 64 or 48 for
    /// IPv6. Absent with the address.
    #[serde(default)]
    pub prefix: Option<u8>,

    /// What was being guessed at: a username for [`LockoutClass::Account`], a
    /// client identifier for [`LockoutClass::Client`], the endpoint's own
    /// name (`passkey`, `setup-token`, …) for [`LockoutClass::Address`], and
    /// the address with its prefix (`198.51.100.4/32`, `2001:db8:1::/48`, or
    /// `unknown`) for [`LockoutClass::Source`]. Folded to lower case.
    pub key: String,

    /// When the failure that crossed the limit happened.
    pub started_at: DateTime<Utc>,

    /// When the key will be let through again on its own.
    pub ends_at: DateTime<Utc>,

    /// The estimated failures inside the window that earned it: never fewer
    /// than there were, possibly more.
    pub failures: u32,
}

/// One of the limiter's two sketches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockoutTier {
    /// Tier 1, keyed on the address.
    Source,

    /// Tier 2, keyed on the address and what it was guessing at.
    Pair,
}

impl LockoutTier {
    /// A short phrase naming the tier for somebody reading the console.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Source => "Addresses",
            Self::Pair => "Accounts and endpoints",
        }
    }
}

/// How full one sketch is, from a fixed sample of its cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierFill {
    pub tier: LockoutTier,

    /// Cells read.
    pub sampled: u32,

    /// Of those, how many are locked now.
    pub locked: u32,

    /// How many cells a key has — one per row — every one of which must be
    /// locked for the key to be refused.
    pub rows: u32,
}

impl TierFill {
    /// The fraction of cells locked, from zero to one.
    pub fn fraction(&self) -> f64 {
        match self.sampled {
            0 => 0.0,
            sampled => f64::from(self.locked) / f64::from(sampled),
        }
    }

    /// About how likely a key that never failed is to be refused: every one
    /// of its cells locked by somebody else.
    pub fn false_refusal(&self) -> f64 {
        self.fraction().powf(f64::from(self.rows))
    }
}

/// How often one class of key has been refused, since the server started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockoutCounter {
    pub class: LockoutClass,

    /// Attempts turned away because their key was locked out.
    pub refusals: u64,

    /// Lockouts that began.
    pub lockouts: u64,
}

/// `GET /api/v1/auth/lockouts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockouts {
    /// The most recently started lockouts still in force, newest first, at
    /// most [`MAX_LISTED_LOCKOUTS`] of them.
    pub lockouts: Vec<Lockout>,

    /// How many of the recorded lockouts are still in force, which is more
    /// than `lockouts.len()` when the list was cut short. The record holds the
    /// most recent few hundred, so under a flood this is a lower bound; the
    /// `lockouts` counters say how many began.
    pub total: u32,

    /// One entry per [`LockoutClass`], in [`LockoutClass::ALL`] order.
    pub counters: Vec<LockoutCounter>,

    /// When the counters started counting, which is when this process did:
    /// the limiter is in memory, so a restart forgives every lockout and zeroes
    /// every counter.
    pub counting_since: DateTime<Utc>,

    /// How full each tier is.
    #[serde(default)]
    pub tiers: Vec<TierFill>,
}

/// The most lockouts one listing carries.
///
/// A real installation has a handful at a time. A flood has many more, and a
/// page listing all of those would be neither readable nor cheap; `total`
/// says how many there are.
pub const MAX_LISTED_LOCKOUTS: usize = 200;

/// `POST /api/v1/auth/lockouts/clear` — forgives one key, now.
///
/// The three fields of the [`Lockout`] being cleared, exactly as the listing
/// gave them. Clearing zeroes the key's cells, which also forgives any other
/// key that shares every one of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearLockoutRequest {
    pub class: LockoutClass,

    #[serde(default)]
    pub address: Option<IpAddr>,

    pub key: String,
}

/// What `POST /api/v1/auth/lockouts/clear` answers: the lockout that was
/// cleared, with its fields at the top level, and what clearing it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearedLockout {
    #[serde(flatten)]
    pub lockout: Lockout,

    /// That clearing zeroed the key's cells, and so also forgave any other key
    /// whose cells were all among them.
    pub note: String,
}

/// The [`ClearedLockout::note`] the server sends.
pub const CLEARED_NOTE: &str = "Cleared. The limiter counts in shared cells, so this also \
    forgave any other key whose cells were all among this one's.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_has_a_wire_name_a_label_and_its_own_slot() {
        for (position, class) in LockoutClass::ALL.iter().enumerate() {
            assert_eq!(class.index(), position);
            assert!(!class.label().is_empty());

            let wire = serde_json::to_value(class).unwrap();
            assert_eq!(wire, serde_json::json!(class.as_str()));
        }
    }

    #[test]
    fn a_lockout_is_written_the_way_the_console_reads_it() {
        let lockout = Lockout {
            class: LockoutClass::Account,
            address: Some("198.51.100.4".parse().unwrap()),
            prefix: Some(32),
            key: "ada".to_string(),
            started_at: "2026-09-29T10:00:00Z".parse().unwrap(),
            ends_at: "2026-09-29T10:15:00Z".parse().unwrap(),
            failures: 10,
        };

        let json = serde_json::to_value(&lockout).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "class": "account",
                "address": "198.51.100.4",
                "prefix": 32,
                "key": "ada",
                "started_at": "2026-09-29T10:00:00Z",
                "ends_at": "2026-09-29T10:15:00Z",
                "failures": 10,
            })
        );
        assert_eq!(serde_json::from_value::<Lockout>(json).unwrap(), lockout);
    }

    #[test]
    fn a_cleared_lockout_is_the_lockout_with_a_note_beside_it() {
        let cleared = ClearedLockout {
            lockout: Lockout {
                class: LockoutClass::Source,
                address: Some("2001:db8:1::".parse().unwrap()),
                prefix: Some(48),
                key: "2001:db8:1::/48".to_string(),
                started_at: "2026-09-29T10:00:00Z".parse().unwrap(),
                ends_at: "2026-09-29T10:15:00Z".parse().unwrap(),
                failures: 300,
            },
            note: CLEARED_NOTE.to_string(),
        };

        let json = serde_json::to_value(&cleared).unwrap();

        assert_eq!(json["class"], "source");
        assert_eq!(json["key"], "2001:db8:1::/48");
        assert_eq!(json["note"], CLEARED_NOTE);
        assert_eq!(
            serde_json::from_value::<Lockout>(json.clone()).unwrap(),
            cleared.lockout,
            "a client that only knows a lockout still reads one",
        );
        assert_eq!(
            serde_json::from_value::<ClearedLockout>(json).unwrap(),
            cleared
        );
    }

    #[test]
    fn a_tier_says_how_full_it_is_and_what_that_costs_a_stranger() {
        let fill = TierFill {
            tier: LockoutTier::Pair,
            sampled: 4096,
            locked: 2048,
            rows: 4,
        };

        assert_eq!(fill.fraction(), 0.5);
        assert_eq!(fill.false_refusal(), 0.0625);
        assert_eq!(
            serde_json::to_value(fill).unwrap(),
            serde_json::json!({"tier": "pair", "sampled": 4096, "locked": 2048, "rows": 4}),
        );
        assert_eq!(TierFill { sampled: 0, ..fill }.fraction(), 0.0);
    }
}
