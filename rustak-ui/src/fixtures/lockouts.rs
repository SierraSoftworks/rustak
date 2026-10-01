//! Demo data for the sign-in lockouts card.
//!
//! One lockout of each class, so every way the card describes a key is on
//! screen at once, held in a thread-local store so that clearing one takes it
//! off the list exactly as the server's does — and clearing it again is the
//! server's `404`, not a second success.
//!
//! Every address is from the documentation ranges (RFC 5737), so a demo never
//! shows anybody a real one.

use std::cell::RefCell;

use chrono::Duration;
use rustak_api::{
    CLEARED_NOTE, ClearLockoutRequest, ClearedLockout, Lockout, LockoutClass, LockoutCounter,
    LockoutTier, Lockouts, TierFill,
};

use super::data::ago;
use crate::api::ApiError;

thread_local! {
    static LOCKOUTS: RefCell<Vec<Lockout>> = RefCell::new(seed());
}

/// The lockouts a fresh demo starts with, newest first.
fn seed() -> Vec<Lockout> {
    let lockout = |class, address: &str, key: &str, minutes_ago: i64, failures| {
        let started_at = ago(minutes_ago);

        Lockout {
            class,
            address: address.parse().ok(),
            prefix: Some(32),
            key: key.to_string(),
            started_at,
            ends_at: started_at + Duration::minutes(15),
            failures,
        }
    };

    vec![
        lockout(LockoutClass::Account, "203.0.113.7", "linus", 2, 10),
        lockout(
            LockoutClass::Source,
            "203.0.113.99",
            "203.0.113.99/32",
            4,
            300,
        ),
        lockout(LockoutClass::Address, "198.51.100.23", "passkey", 6, 11),
        lockout(LockoutClass::Client, "192.0.2.10", "cloudtak", 9, 10),
    ]
}

/// What the limiter is refusing now.
pub fn lockouts() -> Lockouts {
    let lockouts = LOCKOUTS.with(|held| held.borrow().clone());
    let counts = |class, refusals, lockouts| LockoutCounter {
        class,
        refusals,
        lockouts,
    };

    Lockouts {
        total: u32::try_from(lockouts.len()).unwrap_or(u32::MAX),
        lockouts,
        counters: vec![
            counts(LockoutClass::Address, 4, 1),
            counts(LockoutClass::Account, 37, 3),
            counts(LockoutClass::Client, 0, 1),
            counts(LockoutClass::Source, 12, 1),
        ],
        counting_since: ago(60 * 26),
        tiers: vec![
            TierFill {
                tier: LockoutTier::Source,
                sampled: 4096,
                locked: 4,
                rows: 4,
            },
            TierFill {
                tier: LockoutTier::Pair,
                sampled: 4096,
                locked: 12,
                rows: 4,
            },
        ],
    }
}

/// Forgives one, as the server does: once.
pub fn clear_lockout(request: &ClearLockoutRequest) -> Result<ClearedLockout, ApiError> {
    LOCKOUTS
        .with(|held| take(&mut held.borrow_mut(), request))
        .map(|lockout| ClearedLockout {
            lockout,
            note: CLEARED_NOTE.to_string(),
        })
}

/// Takes the matching lockout out of `held`.
fn take(held: &mut Vec<Lockout>, request: &ClearLockoutRequest) -> Result<Lockout, ApiError> {
    let position = held
        .iter()
        .position(|lockout| {
            lockout.class == request.class
                && lockout.address == request.address
                && lockout.key == request.key
        })
        .ok_or_else(|| {
            ApiError::Server(
                "Nothing is locked out under that key. It may have run out on its own.".to_string(),
            )
        })?;

    Ok(held.remove(position))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_for(lockout: &Lockout) -> ClearLockoutRequest {
        ClearLockoutRequest {
            class: lockout.class,
            address: lockout.address,
            key: lockout.key.clone(),
        }
    }

    #[test]
    fn the_demo_shows_one_lockout_of_every_class() {
        let seeded = seed();

        for class in LockoutClass::ALL {
            assert_eq!(
                seeded
                    .iter()
                    .filter(|lockout| lockout.class == class)
                    .count(),
                1,
                "{class:?}",
            );
        }
        assert!(seeded.iter().all(|lockout| lockout.address.is_some()));
    }

    #[test]
    fn a_lockout_is_cleared_once_and_the_second_try_is_refused() {
        let mut held = seed();
        let first = held[0].clone();

        assert_eq!(take(&mut held, &request_for(&first)).unwrap(), first);
        assert_eq!(held.len(), 3);
        assert!(take(&mut held, &request_for(&first)).is_err());
    }
}
