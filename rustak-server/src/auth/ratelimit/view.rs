//! Reading the limiter from the outside: what is locked out, and forgiving it.
//!
//! Both are for an administrator, through `/api/v1/auth/lockouts`, and both
//! are careful about the one thing a view of a limiter must not do, which is
//! change it by looking. Listing reads the ring of lockout events and asks the
//! sketch, for each, whether its key is still refused — it writes nothing, so
//! looking cannot extend, create or end a lockout. Clearing zeroes one key's
//! cells, and only a key the sketch refuses now; because cells are shared,
//! that also forgives any other key whose cells were all among them.

use std::collections::HashSet;
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use rustak_api::{Lockout, LockoutClass, LockoutTier, Lockouts, TierFill};

use super::events::{Counted, Event, Tier};
use super::key::Source;
use super::sketch::DEPTH;
use super::{RateLimiter, subject_for};

/// Why a key could not be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unclearable {
    /// The class and key are not a pair the limiter could have counted.
    NotAKey,
    /// The key is not refused now, so there is nothing to forgive.
    NotLocked,
}

impl RateLimiter {
    /// The keys locked out now, newest first, how many there are, and how
    /// full each tier is.
    pub fn lockouts(&self, limit: usize) -> Lockouts {
        self.lockouts_at(Utc::now(), limit)
    }

    /// [`lockouts`](Self::lockouts), at a moment the caller chooses.
    pub(super) fn lockouts_at(&self, now: DateTime<Utc>, limit: usize) -> Lockouts {
        let stamp = self.clock.stamp(now);
        let mut seen = HashSet::new();

        // Newest first, so a key locked twice is described by its latest.
        let live: Vec<Lockout> = self
            .ring
            .newest_first()
            .into_iter()
            .filter(|event| !event.cleared && seen.insert((event.key.tier, event.key.hash)))
            .filter_map(|event| {
                let until = self
                    .table(event.key.tier)
                    .locked_until(event.key.hash, stamp)?;

                Some(self.describe(&event.key, event.since, until, event.estimate))
            })
            .collect();

        let total = live.len();

        Lockouts {
            lockouts: live.into_iter().take(limit).collect(),
            total: u32::try_from(total).unwrap_or(u32::MAX),
            counters: self.counters.snapshot(),
            counting_since: self.created_at,
            tiers: [Tier::Source, Tier::Pair]
                .map(|tier| {
                    let (locked, sampled) = self.table(tier).locked_sample(stamp);

                    TierFill {
                        tier: match tier {
                            Tier::Source => LockoutTier::Source,
                            Tier::Pair => LockoutTier::Pair,
                        },
                        sampled,
                        locked,
                        rows: DEPTH as u32,
                    }
                })
                .to_vec(),
        }
    }

    /// Forgives one key that is locked out now, as the listing named it.
    ///
    /// The key's cells are zeroed, count and lock alike: a key forgiven with
    /// nine failures still counted would be locked out again by its next
    /// mistake. A clear cannot be used to reset a count that has not reached
    /// its limit, because only a key refused now is cleared.
    ///
    /// # Errors
    ///
    /// [`Unclearable::NotAKey`] for a class and key the limiter could not have
    /// counted, [`Unclearable::NotLocked`] when the key is not refused now.
    pub fn clear(
        &self,
        class: LockoutClass,
        address: Option<IpAddr>,
        key: &str,
    ) -> Result<Lockout, Unclearable> {
        self.clear_at(Utc::now(), class, address, key)
    }

    /// [`clear`](Self::clear), at a moment the caller chooses.
    pub(super) fn clear_at(
        &self,
        now: DateTime<Utc>,
        class: LockoutClass,
        address: Option<IpAddr>,
        key: &str,
    ) -> Result<Lockout, Unclearable> {
        let counted = match class {
            LockoutClass::Source => {
                let source = Source::parse_shown(key).ok_or(Unclearable::NotAKey)?;
                if source.address() != address {
                    return Err(Unclearable::NotAKey);
                }

                self.source_key(source)
            }
            _ => {
                let subject = subject_for(class, key).ok_or(Unclearable::NotAKey)?;

                self.pair_key(Source::host(address), &subject)
            }
        };

        let stamp = self.clock.stamp(now);
        let table = self.table(counted.tier);
        let until = table
            .locked_until(counted.hash, stamp)
            .ok_or(Unclearable::NotLocked)?;
        let estimate = table.estimate(counted.hash, stamp.window);

        table.clear(counted.hash);

        // The event says when it began; a key whose event has left the ring
        // began a whole lockout before it ends, as the sketch has it, and its
        // estimate is what its cells hold for this window.
        let started = self
            .ring
            .clear(counted.tier, counted.hash)
            .unwrap_or_else(|| {
                Event::new(
                    counted,
                    self.clock.at(until.saturating_sub(self.clock.lockout())),
                    estimate,
                )
            });

        Ok(self.describe(&counted, started.since, until, started.estimate))
    }

    /// One lockout as the API describes it.
    fn describe(&self, key: &Counted, since: DateTime<Utc>, until: u32, estimate: u16) -> Lockout {
        let (class, shown) = match &key.subject {
            Some(subject) => {
                let (class, shown) = super::classify(subject.as_str());
                let ellipsis = if subject.is_cut() { "…" } else { "" };

                (class, format!("{shown}{ellipsis}"))
            }
            None => (LockoutClass::Source, key.source.shown()),
        };

        Lockout {
            class,
            address: key.source.address(),
            prefix: key.source.prefix(),
            key: shown,
            started_at: since,
            ends_at: self.clock.at(until),
            failures: u32::from(estimate),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::auth::ratelimit::key::KeyedHash;
    use crate::config::RateLimitConfig;

    fn limiter() -> (RateLimiter, DateTime<Utc>) {
        let at = Utc::now();
        // Every allowance explicit, so a change of default moves none of
        // these tests.
        let config = RateLimitConfig {
            attempts: 3,
            address_attempts: 12,
            network_attempts: 36,
            ..RateLimitConfig::default()
        };

        (
            RateLimiter::build(&config, 1 << 12, KeyedHash::fixed(7, 11), at),
            at,
        )
    }

    fn address() -> Option<IpAddr> {
        Some("198.51.100.4".parse().unwrap())
    }

    fn lock_out(limiter: &RateLimiter, at: DateTime<Utc>, subject: &str) {
        for _ in 0..3 {
            limiter.record_failures_at(at, address(), &[subject]);
        }
    }

    #[test]
    fn a_lockout_is_listed_with_when_it_began_when_it_ends_and_its_estimate() {
        let (limiter, at) = limiter();

        limiter.record_failures_at(at, address(), &["grace"]);
        lock_out(&limiter, at, "Ada");

        let listed = limiter.lockouts_at(at, 10);

        assert_eq!(
            listed.total, 1,
            "a key with failures but no lockout is not one"
        );
        assert_eq!(
            listed.lockouts,
            vec![Lockout {
                class: LockoutClass::Account,
                address: address(),
                prefix: Some(32),
                key: "ada".to_string(),
                started_at: at,
                ends_at: limiter
                    .clock
                    .at(limiter.clock.lockout_ends(limiter.clock.stamp(at))),
                failures: 3,
            }]
        );
        assert!(listed.lockouts[0].ends_at > at + Duration::minutes(14));
        assert_eq!(listed.tiers.len(), 2);
        assert!(
            listed
                .tiers
                .iter()
                .all(|tier| tier.sampled == 4096 && tier.rows == 4)
        );
    }

    #[test]
    fn a_tier_one_lockout_is_listed_as_the_address_it_is() {
        let (limiter, at) = limiter();

        for index in 0..12 {
            limiter.record_failures_at(at, address(), &[&format!("user-{index}")]);
        }

        let listed = limiter.lockouts_at(at, 10);
        let source = &listed.lockouts[0];

        assert_eq!(source.class, LockoutClass::Source);
        assert_eq!(source.key, "198.51.100.4/32");
        assert_eq!(source.address, address());
        assert_eq!(source.failures, 12);
    }

    #[test]
    fn looking_neither_extends_nor_ends_nor_creates_a_lockout() {
        let (limiter, at) = limiter();

        lock_out(&limiter, at, "ada");
        let before = limiter.lockouts_at(at, 10);

        for _ in 0..5 {
            limiter.lockouts_at(at + Duration::minutes(1), 10);
        }

        assert_eq!(limiter.lockouts_at(at, 10), before);
        assert!(limiter.check_at(at, address(), "ada").is_err());
        assert_eq!(
            limiter.counters.snapshot()[LockoutClass::Account.index()].refusals,
            1,
            "only the check above was a refusal; reading the list is not one",
        );
    }

    #[test]
    fn an_expired_lockout_is_not_listed() {
        let (limiter, at) = limiter();

        lock_out(&limiter, at, "ada");

        assert_eq!(limiter.lockouts_at(at + Duration::minutes(16), 10).total, 0);
    }

    #[test]
    fn the_listing_is_bounded_newest_first_and_says_how_many_it_left_out() {
        let (limiter, at) = limiter();

        for index in 0..8 {
            let address: IpAddr = format!("198.51.100.{index}").parse().unwrap();
            for _ in 0..3 {
                limiter.record_failures_at(
                    at + Duration::seconds(index),
                    Some(address),
                    &[&format!("user-{index}")],
                );
            }
        }

        let listed = limiter.lockouts_at(at + Duration::seconds(30), 5);

        assert_eq!(listed.total, 8);
        assert_eq!(
            listed
                .lockouts
                .iter()
                .map(|lockout| lockout.key.as_str())
                .collect::<Vec<_>>(),
            ["user-7", "user-6", "user-5", "user-4", "user-3"],
        );
    }

    #[test]
    fn clearing_forgives_the_key_and_its_failures_and_takes_it_off_the_list() {
        let (limiter, at) = limiter();

        lock_out(&limiter, at, "ada");

        let cleared = limiter
            .clear_at(at, LockoutClass::Account, address(), "ADA")
            .unwrap();

        assert_eq!(cleared.key, "ada");
        assert_eq!(cleared.started_at, at);
        assert!(limiter.check_at(at, address(), "ada").is_ok());
        assert_eq!(limiter.lockouts_at(at, 10).total, 0);
        assert_eq!(
            limiter.record_failures_at(at, address(), &["ada"]),
            None,
            "a cleared key starts counting from nothing",
        );
    }

    #[test]
    fn clearing_touches_one_key_and_only_a_locked_one() {
        let (limiter, at) = limiter();

        lock_out(&limiter, at, "ada");
        lock_out(&limiter, at, "grace");
        limiter.record_failures_at(at, address(), &["linus"]);

        let clear = |class, address, key| limiter.clear_at(at, class, address, key);

        assert_eq!(
            clear(LockoutClass::Account, None, "ada"),
            Err(Unclearable::NotLocked),
            "another address",
        );
        assert_eq!(
            clear(LockoutClass::Account, address(), "linus"),
            Err(Unclearable::NotLocked),
        );
        assert_eq!(
            clear(LockoutClass::Account, address(), "passkey"),
            Err(Unclearable::NotAKey),
        );
        assert!(clear(LockoutClass::Account, address(), "ada").is_ok());

        assert!(limiter.check_at(at, address(), "grace").is_err());
    }

    #[test]
    fn an_address_is_cleared_by_the_key_it_was_listed_under() {
        let (limiter, at) = limiter();

        for index in 0..12 {
            limiter.record_failures_at(at, address(), &[&format!("user-{index}")]);
        }
        assert!(limiter.check_at(at, address(), "anybody").is_err());

        let clear = |address, key| limiter.clear_at(at, LockoutClass::Source, address, key);

        assert_eq!(
            clear(address(), "198.51.100.4/24"),
            Err(Unclearable::NotAKey)
        );
        assert_eq!(clear(None, "198.51.100.4/32"), Err(Unclearable::NotAKey));
        assert_eq!(clear(address(), "198.51.100.4/32").unwrap().failures, 12);
        assert!(limiter.check_at(at, address(), "anybody").is_ok());
    }

    #[test]
    fn a_key_whose_event_left_the_ring_is_still_cleared_and_described() {
        let (limiter, at) = limiter();

        lock_out(&limiter, at, "ada");
        for event in 0..(crate::auth::ratelimit::events::CAPACITY as u64) {
            let mut counted = limiter.source_key(Source::host(None));
            counted.hash = event;
            limiter.ring.push(Event::new(counted, at, 1));
        }

        let cleared = limiter
            .clear_at(
                at + Duration::seconds(30),
                LockoutClass::Account,
                address(),
                "ada",
            )
            .unwrap();

        assert_eq!(cleared.started_at, cleared.ends_at - Duration::minutes(15));
        assert_eq!(cleared.failures, 3);
    }
}
