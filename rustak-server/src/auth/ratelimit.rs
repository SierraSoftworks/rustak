//! A bucket per (address, subject) pair, so guessing costs time.
//!
//! Every endpoint that accepts a secret — a passkey assertion, a refresh token,
//! a setup token — goes through here first. Ten failures inside a minute lock
//! that pair out for fifteen, which is long enough to make an online guessing
//! attack pointless and short enough that somebody who mistyped a token can try
//! again after a coffee.
//!
//! # Why in memory
//!
//! rustak is a single process, so a `Mutex<HashMap<_, _>>` is the whole of what
//! a shared counter needs, and it costs nothing when nobody is failing. Putting
//! it in SQLite would add a write to the *hot* path — every successful sign-in
//! would clear a row — to buy persistence across restarts that an attacker
//! cannot cause and a legitimate user would only notice as their lockout being
//! forgiven early.
//!
//! The map is swept on [`RateLimiter::check`] rather than by a timer: the only
//! thing that grows it is failures, so the only place it needs pruning is where
//! a failure is looked up.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

use crate::config::RateLimitConfig;

/// How many entries may accumulate before a sweep is forced.
///
/// A sweep is linear in the size of the map, so doing one on every failure
/// would be quadratic under attack. This bounds the memory a flood of distinct
/// addresses can cost us without putting the scan in the common path.
const SWEEP_AT: usize = 1024;

/// What a key is doing right now.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Failures counted since `window_started`.
    failures: u32,
    /// When the current counting window began.
    window_started: DateTime<Utc>,
    /// When the lockout ends, for a key that has one.
    locked_until: Option<DateTime<Utc>>,
}

/// Refuses a key that has been failing.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<(Option<IpAddr>, String), Bucket>>,
    attempts: u32,
    window: Duration,
    lockout: Duration,
}

impl RateLimiter {
    /// Builds a limiter from `[auth.rate_limit]`.
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            attempts: config.attempts.max(1),
            window: config.window,
            lockout: config.lockout,
        }
    }

    /// Whether this key may attempt now, and for how long it may not.
    ///
    /// The subject is whatever the endpoint is guessing at — a username, a
    /// challenge id, the literal `setup` — so that failures against one account
    /// do not lock out another from the same office.
    pub fn check(&self, client_ip: Option<IpAddr>, subject: &str) -> Result<(), Duration> {
        let now = Utc::now();
        let mut buckets = self.lock();

        if buckets.len() > SWEEP_AT {
            buckets.retain(|_, bucket| live(bucket, now, self.window));
        }

        match buckets.get(&(client_ip, subject.to_owned())) {
            Some(Bucket {
                locked_until: Some(until),
                ..
            }) if *until > now => Err(*until - now),
            _ => Ok(()),
        }
    }

    /// Counts a failure, locking the key out once it has had too many.
    ///
    /// Returns the lockout that was applied, so the caller can log or audit the
    /// moment it starts rather than discovering it on the next attempt.
    pub fn record_failure(&self, client_ip: Option<IpAddr>, subject: &str) -> Option<Duration> {
        let now = Utc::now();
        let mut buckets = self.lock();

        let bucket = buckets
            .entry((client_ip, subject.to_owned()))
            .or_insert(Bucket {
                failures: 0,
                window_started: now,
                locked_until: None,
            });

        // A window that has run out starts again rather than accumulating: the
        // limit is ten failures a minute, not ten failures ever.
        if now - bucket.window_started > self.window {
            bucket.failures = 0;
            bucket.window_started = now;
        }

        bucket.failures += 1;

        if bucket.failures >= self.attempts && bucket.locked_until.is_none_or(|until| until <= now)
        {
            bucket.locked_until = Some(now + self.lockout);
            return Some(self.lockout);
        }

        None
    }

    /// Forgets a key, which is what a success means.
    pub fn record_success(&self, client_ip: Option<IpAddr>, subject: &str) {
        self.lock().remove(&(client_ip, subject.to_owned()));
    }

    /// Drops every key that is neither locked out nor inside its window.
    pub fn sweep(&self) {
        let now = Utc::now();

        self.lock()
            .retain(|_, bucket| live(bucket, now, self.window));
    }

    /// How many keys are being tracked, for the tests and for diagnostics.
    pub fn tracked(&self) -> usize {
        self.lock().len()
    }

    /// The map, recovering from a panic in another holder.
    ///
    /// A poisoned lock here means some other thread panicked while counting a
    /// failure. Refusing every sign-in from then on would turn that into an
    /// outage; the worst a recovered map can be is a count that is off by one.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(Option<IpAddr>, String), Bucket>> {
        self.buckets.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Whether a bucket still says anything about the future.
fn live(bucket: &Bucket, now: DateTime<Utc>, window: Duration) -> bool {
    bucket.locked_until.is_some_and(|until| until > now) || now - bucket.window_started <= window
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(attempts: u32) -> RateLimiter {
        RateLimiter::new(&RateLimitConfig {
            attempts,
            window: Duration::minutes(1),
            lockout: Duration::minutes(15),
        })
    }

    fn address() -> Option<IpAddr> {
        Some("198.51.100.4".parse().unwrap())
    }

    #[test]
    fn a_key_with_no_history_is_let_through() {
        assert!(limiter(3).check(address(), "ada").is_ok());
    }

    #[test]
    fn the_lockout_starts_on_the_attempt_that_crosses_the_limit() {
        let limiter = limiter(3);

        assert_eq!(limiter.record_failure(address(), "ada"), None);
        assert_eq!(limiter.record_failure(address(), "ada"), None);
        assert!(limiter.check(address(), "ada").is_ok());

        assert_eq!(
            limiter.record_failure(address(), "ada"),
            Some(Duration::minutes(15))
        );

        let remaining = limiter.check(address(), "ada").unwrap_err();
        assert!(remaining <= Duration::minutes(15) && remaining > Duration::minutes(14));
    }

    #[test]
    fn a_lockout_is_reported_once_rather_than_extended_by_every_later_attempt() {
        // Otherwise an attacker who keeps hammering would keep resetting their
        // own lockout — which is harmless — but we would also keep writing an
        // audit entry per attempt, which is not.
        let limiter = limiter(1);

        assert!(limiter.record_failure(address(), "ada").is_some());
        assert_eq!(limiter.record_failure(address(), "ada"), None);
    }

    #[test]
    fn failures_against_one_account_do_not_lock_out_another() {
        let limiter = limiter(1);

        limiter.record_failure(address(), "ada");

        assert!(limiter.check(address(), "grace").is_ok());
        assert!(limiter.check(None, "ada").is_ok());
    }

    #[test]
    fn a_success_forgets_what_came_before_it() {
        let limiter = limiter(3);

        limiter.record_failure(address(), "ada");
        limiter.record_failure(address(), "ada");
        limiter.record_success(address(), "ada");

        assert_eq!(limiter.tracked(), 0);
        assert_eq!(limiter.record_failure(address(), "ada"), None);
    }

    #[test]
    fn sweeping_keeps_the_keys_that_still_mean_something() {
        let limiter = limiter(1);

        limiter.record_failure(address(), "locked-out");
        limiter.sweep();

        assert_eq!(limiter.tracked(), 1, "a live lockout must survive a sweep");
        assert!(limiter.check(address(), "locked-out").is_err());
    }

    #[test]
    fn a_window_that_has_run_out_starts_counting_again() {
        // The limit is ten failures a minute, not ten failures ever: somebody
        // who mistypes a token twice a day should never be locked out.
        let limiter = RateLimiter::new(&RateLimitConfig {
            attempts: 2,
            window: Duration::milliseconds(30),
            lockout: Duration::minutes(15),
        });

        assert_eq!(limiter.record_failure(address(), "ada"), None);

        std::thread::sleep(std::time::Duration::from_millis(60));

        assert_eq!(
            limiter.record_failure(address(), "ada"),
            None,
            "a failure in a new window is the first one, not the second",
        );
        assert!(limiter.check(address(), "ada").is_ok());
    }
}
