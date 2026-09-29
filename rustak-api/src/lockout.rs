//! What the sign-in rate limiter is refusing right now, and why.
//!
//! Every endpoint that accepts a secret counts its failures against a key made
//! of the caller's address and what it was guessing at. Too many inside a
//! window lock that key out for a while. Before this existed, the only trace a
//! lockout left was the `429` its victim received, so an administrator asking
//! "why was I refused?" had nothing to look at (M9-14).
//!
//! # Classes, not keys, are what gets counted
//!
//! A [`LockoutClass`] says what kind of thing the key names — an account, an
//! OAuth client, or a sign-in endpoint that is only told apart by the address
//! calling it. The counters in [`Lockouts`] are per class and never per key, so
//! nothing that reads them learns an address or a username. The list of
//! current lockouts does carry both, which is why it is for administrators
//! only.

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
}

impl LockoutClass {
    /// Every class, in the order a reader is offered them.
    pub const ALL: [Self; 3] = [Self::Address, Self::Account, Self::Client];

    /// The value carried on the wire, and the only label a counter ever has.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Account => "account",
            Self::Client => "client",
        }
    }

    /// A short phrase naming the class for somebody reading the console.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Address => "Address",
            Self::Account => "Account",
            Self::Client => "OAuth client",
        }
    }

    /// This class's position in [`ALL`](Self::ALL), for an array of counters.
    pub fn index(&self) -> usize {
        match self {
            Self::Address => 0,
            Self::Account => 1,
            Self::Client => 2,
        }
    }
}

/// One key the limiter is refusing now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockout {
    pub class: LockoutClass,

    /// Where the failures came from. Absent when the server could not tell,
    /// which in practice is a request that reached it over a Unix socket.
    #[serde(default)]
    pub address: Option<IpAddr>,

    /// What was being guessed at: a username for [`LockoutClass::Account`], a
    /// client identifier for [`LockoutClass::Client`], and the endpoint's own
    /// name (`passkey`, `setup-token`, …) for [`LockoutClass::Address`].
    pub key: String,

    /// When the failure that crossed the limit happened.
    pub started_at: DateTime<Utc>,

    /// When the key will be let through again on its own.
    pub ends_at: DateTime<Utc>,

    /// How many failures inside the window earned it.
    pub failures: u32,
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
    /// The most recently started lockouts, newest first, at most
    /// [`MAX_LISTED_LOCKOUTS`] of them.
    pub lockouts: Vec<Lockout>,

    /// How many keys are locked out in all, which is more than
    /// `lockouts.len()` when the list was cut short.
    pub total: u32,

    /// One entry per [`LockoutClass`], in [`LockoutClass::ALL`] order.
    pub counters: Vec<LockoutCounter>,

    /// When the counters started counting, which is when this process did:
    /// the limiter is in memory, so a restart forgives every lockout and zeroes
    /// every counter.
    pub counting_since: DateTime<Utc>,
}

/// The most lockouts one listing carries.
///
/// A real installation has a handful at a time. A flood has up to the
/// limiter's ceiling of a hundred thousand, and a page listing all of those
/// would be neither readable nor cheap; `total` says how many there are.
pub const MAX_LISTED_LOCKOUTS: usize = 200;

/// `POST /api/v1/auth/lockouts/clear` — forgives one key, now.
///
/// The three fields of the [`Lockout`] being cleared, exactly as the listing
/// gave them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearLockoutRequest {
    pub class: LockoutClass,

    #[serde(default)]
    pub address: Option<IpAddr>,

    pub key: String,
}

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
                "key": "ada",
                "started_at": "2026-09-29T10:00:00Z",
                "ends_at": "2026-09-29T10:15:00Z",
                "failures": 10,
            })
        );
        assert_eq!(serde_json::from_value::<Lockout>(json).unwrap(), lockout);
    }
}
