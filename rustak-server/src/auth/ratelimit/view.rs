//! Reading the limiter from the outside: what is locked out, and forgiving it.
//!
//! Both are for an administrator, through `/api/v1/auth/lockouts`, and both are
//! careful about the one thing a view of a limiter must not do, which is change
//! it. Listing never sweeps, never inserts and never touches a bucket's times,
//! so looking cannot extend, create or end a lockout. Clearing removes exactly
//! one bucket, and only one that is locked out now.

use std::cmp::Reverse;
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use rustak_api::{Lockout, Lockouts};

use super::{Key, Lock, RateLimiter, classify};

impl RateLimiter {
    /// The keys locked out now, newest first, and how many there are in all.
    ///
    /// At most `limit` are described. Under a flood the map holds up to
    /// [`MAX_BUCKETS`](super::MAX_BUCKETS) lockouts; the scan over them is one
    /// pass under the mutex with no allocation per entry, and only the `limit`
    /// kept are cloned.
    pub fn lockouts(&self, limit: usize) -> Lockouts {
        self.lockouts_at(Utc::now(), limit)
    }

    /// [`lockouts`](Self::lockouts), at a moment the caller chooses.
    pub(super) fn lockouts_at(&self, now: DateTime<Utc>, limit: usize) -> Lockouts {
        let buckets = self.lock();

        let mut active: Vec<(&Key, Lock)> = buckets
            .map
            .iter()
            .filter_map(|(key, bucket)| bucket.active(now).map(|lock| (key, lock)))
            .collect();
        let total = active.len();

        // Newest first; the tie-break on the key keeps the order stable for a
        // page that re-reads it.
        let newest = |a: &(&Key, Lock), b: &(&Key, Lock)| {
            (Reverse(a.1.since), a.0).cmp(&(Reverse(b.1.since), b.0))
        };
        if active.len() > limit {
            active.select_nth_unstable_by(limit, newest);
            active.truncate(limit);
        }
        active.sort_unstable_by(newest);

        let lockouts = active
            .into_iter()
            .map(|((address, subject), lock)| describe(*address, subject, lock))
            .collect();

        drop(buckets);

        Lockouts {
            lockouts,
            total: u32::try_from(total).unwrap_or(u32::MAX),
            counters: self.counters.snapshot(),
            counting_since: self.created_at,
        }
    }

    /// Forgives one key that is locked out now.
    ///
    /// The whole bucket goes, failures and all, exactly as a success would
    /// have removed it: a key forgiven with nine failures still counted would
    /// be locked out again by its next mistake. [`None`] when the key is not
    /// locked out — whether it has a few failures against it or none — so a
    /// clear cannot be used to reset somebody's count before they reach it.
    pub fn clear(&self, address: Option<IpAddr>, subject: &str) -> Option<Lockout> {
        self.clear_at(Utc::now(), address, subject)
    }

    /// [`clear`](Self::clear), at a moment the caller chooses.
    pub(super) fn clear_at(
        &self,
        now: DateTime<Utc>,
        address: Option<IpAddr>,
        subject: &str,
    ) -> Option<Lockout> {
        let mut buckets = self.lock();
        let key = (address, subject.to_owned());
        let lock = buckets.map.get(&key)?.active(now)?;

        buckets.map.remove(&key);

        Some(describe(address, subject, lock))
    }
}

/// One lockout as the API describes it.
fn describe(address: Option<IpAddr>, subject: &str, lock: Lock) -> Lockout {
    let (class, key) = classify(subject);

    Lockout {
        class,
        address,
        key: key.to_owned(),
        started_at: lock.since,
        ends_at: lock.until,
        failures: lock.failures,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use rustak_api::LockoutClass;

    use super::*;
    use crate::config::RateLimitConfig;

    fn limiter() -> RateLimiter {
        RateLimiter::new(&RateLimitConfig {
            attempts: 3,
            window: Duration::minutes(1),
            lockout: Duration::minutes(15),
        })
    }

    fn address() -> Option<IpAddr> {
        Some("198.51.100.4".parse().unwrap())
    }

    fn lock_out(limiter: &RateLimiter, at: DateTime<Utc>, subject: &str) {
        for _ in 0..3 {
            limiter.record_failure_at(at, address(), subject);
        }
    }

    #[test]
    fn a_lockout_is_listed_with_when_it_began_when_it_ends_and_what_earned_it() {
        let limiter = limiter();
        let at = Utc::now();

        limiter.record_failure_at(at, address(), "grace");
        lock_out(&limiter, at, "ada");

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
                key: "ada".to_string(),
                started_at: at,
                ends_at: at + Duration::minutes(15),
                failures: 3,
            }]
        );
    }

    #[test]
    fn looking_neither_extends_nor_ends_nor_creates_a_lockout() {
        let limiter = limiter();
        let at = Utc::now();

        lock_out(&limiter, at, "ada");
        let before = limiter.tracked();

        for _ in 0..5 {
            limiter.lockouts_at(at + Duration::minutes(1), 10);
        }

        assert_eq!(limiter.tracked(), before);
        assert_eq!(
            limiter.lockouts_at(at, 10).lockouts[0].ends_at,
            at + Duration::minutes(15)
        );
        assert!(limiter.check_at(at, address(), "ada").is_err());
        assert_eq!(
            limiter.counters.snapshot()[LockoutClass::Account.index()].refusals,
            1,
            "only the check above was a refusal; reading the list is not one",
        );
    }

    #[test]
    fn an_expired_lockout_is_not_listed() {
        let limiter = limiter();
        let at = Utc::now();

        lock_out(&limiter, at, "ada");

        assert_eq!(limiter.lockouts_at(at + Duration::minutes(16), 10).total, 0);
    }

    #[test]
    fn the_listing_is_bounded_newest_first_and_says_how_many_it_left_out() {
        let limiter = limiter();
        let at = Utc::now();

        for index in 0..20 {
            lock_out(
                &limiter,
                at + Duration::seconds(index),
                &format!("user-{index}"),
            );
        }

        let listed = limiter.lockouts_at(at + Duration::minutes(1), 5);

        assert_eq!(listed.total, 20);
        assert_eq!(
            listed
                .lockouts
                .iter()
                .map(|lockout| lockout.key.as_str())
                .collect::<Vec<_>>(),
            ["user-19", "user-18", "user-17", "user-16", "user-15"],
        );
    }

    #[test]
    fn clearing_forgives_the_key_and_its_failures() {
        let limiter = limiter();
        let at = Utc::now();

        lock_out(&limiter, at, "ada");

        let cleared = limiter.clear_at(at, address(), "ada").unwrap();

        assert_eq!(cleared.key, "ada");
        assert!(limiter.check_at(at, address(), "ada").is_ok());
        assert_eq!(
            limiter.record_failure_at(at, address(), "ada"),
            None,
            "a cleared key starts counting from nothing",
        );
    }

    #[test]
    fn clearing_touches_one_key_and_only_a_locked_one() {
        let limiter = limiter();
        let at = Utc::now();

        lock_out(&limiter, at, "ada");
        lock_out(&limiter, at, "grace");
        limiter.record_failure_at(at, address(), "linus");

        assert!(
            limiter.clear_at(at, None, "ada").is_none(),
            "another address"
        );
        assert!(
            limiter.clear_at(at, address(), "linus").is_none(),
            "not locked"
        );
        assert!(limiter.clear_at(at, address(), "ada").is_some());

        assert!(limiter.check_at(at, address(), "grace").is_err());
        assert_eq!(limiter.tracked(), 2, "grace's lockout and linus's failure");
    }
}
