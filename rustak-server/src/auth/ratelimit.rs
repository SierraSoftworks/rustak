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
//! The map is swept from the paths that use it rather than by a timer: the only
//! thing that grows it is failures, so the only place it needs pruning is where
//! a failure is looked up.
//!
//! # Why the sweep is on a clock and the map has a ceiling
//!
//! The sweep used to run on every [`check`](RateLimiter::check) past a
//! thousand entries, and it keeps anything still locked out — a fifteen-minute
//! default. A flood of a hundred thousand distinct subjects from one address
//! therefore left a hundred thousand entries that every sweep looked at and
//! none of which it could remove, so every legitimate sign-in, refresh and
//! passkey ceremony afterwards paid a full scan under the mutex, on an actix
//! worker thread. The cost of the attack was linear; the cost of surviving it
//! was quadratic.
//!
//! Two bounds fix that. The sweep runs at most once per window, so its cost is
//! amortised over time rather than over requests. And the map has a ceiling:
//! once it is full, a new bucket evicts one of a small random sample, cheapest
//! candidate first. Losing somebody's lockout a few minutes early is a far
//! better failure than making every request pay for the attacker's.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

use crate::config::RateLimitConfig;

/// The most keys tracked at once.
///
/// Roughly ten megabytes of buckets and keys at the sizes a username reaches,
/// which is a bound on what a flood can cost. Far above any real installation:
/// five hundred devices failing simultaneously is five hundred entries.
pub const MAX_BUCKETS: usize = 100_000;

/// How many entries an eviction looks at before choosing one.
///
/// `HashMap`'s iteration order is effectively arbitrary, so a small sample is a
/// random sample. Scanning the whole map for the true best candidate would put
/// the cost this exists to avoid back on the insert path.
const EVICTION_SAMPLE: usize = 64;

/// The key a bucket is filed under: who, and what they are guessing at.
type Key = (Option<IpAddr>, String);

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

/// The tracked keys, and when they were last pruned.
#[derive(Debug)]
struct Buckets {
    map: HashMap<Key, Bucket>,
    /// The earliest moment another sweep is worth running.
    next_sweep: DateTime<Utc>,
}

/// Refuses a key that has been failing.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: Mutex<Buckets>,
    attempts: u32,
    window: Duration,
    lockout: Duration,
}

impl RateLimiter {
    /// Builds a limiter from `[auth.rate_limit]`.
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            buckets: Mutex::new(Buckets {
                map: HashMap::new(),
                next_sweep: Utc::now(),
            }),
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

        self.sweep_if_due(&mut buckets, now);

        match buckets.map.get(&(client_ip, subject.to_owned())) {
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
        let key = (client_ip, subject.to_owned());

        if !buckets.map.contains_key(&key) {
            self.make_room(&mut buckets, now);
        }

        let bucket = buckets.map.entry(key).or_insert(Bucket {
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
        self.lock().map.remove(&(client_ip, subject.to_owned()));
    }

    /// Drops every key that is neither locked out nor inside its window.
    pub fn sweep(&self) {
        let now = Utc::now();
        let mut buckets = self.lock();

        self.prune(&mut buckets, now);
    }

    /// How many keys are being tracked, for the tests and for diagnostics.
    pub fn tracked(&self) -> usize {
        self.lock().map.len()
    }

    /// Prunes, but at most once per window.
    ///
    /// The clock is what makes a check O(1): the scan happens on a schedule
    /// rather than on a request, so a map full of entries the scan cannot
    /// remove costs the same as an empty one.
    fn sweep_if_due(&self, buckets: &mut Buckets, now: DateTime<Utc>) {
        if now < buckets.next_sweep {
            return;
        }

        self.prune(buckets, now);
    }

    /// Drops everything expired and sets the next sweep a window away.
    fn prune(&self, buckets: &mut Buckets, now: DateTime<Utc>) {
        buckets
            .map
            .retain(|_, bucket| live(bucket, now, self.window));
        buckets.next_sweep = now + self.window;
    }

    /// Makes space for one more key when the map is at its ceiling.
    ///
    /// Tries the cheap thing first — a prune, which under a flood of locked-out
    /// entries removes nothing — and then gives up a bucket rather than the
    /// bound. An attacker who has filled the map gets their own oldest lockout
    /// forgiven; everybody else keeps a limiter that answers in constant time.
    fn make_room(&self, buckets: &mut Buckets, now: DateTime<Utc>) {
        if buckets.map.len() < MAX_BUCKETS {
            return;
        }

        self.prune(buckets, now);

        while buckets.map.len() >= MAX_BUCKETS {
            let Some(victim) = buckets
                .map
                .iter()
                .take(EVICTION_SAMPLE)
                .min_by_key(|(_, bucket)| bucket.locked_until)
                .map(|(key, _)| key.clone())
            else {
                return;
            };

            buckets.map.remove(&victim);
        }
    }

    /// The map, recovering from a panic in another holder.
    ///
    /// A poisoned lock here means some other thread panicked while counting a
    /// failure. Refusing every sign-in from then on would turn that into an
    /// outage; the worst a recovered map can be is a count that is off by one.
    fn lock(&self) -> std::sync::MutexGuard<'_, Buckets> {
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
    fn a_flood_of_lockouts_does_not_make_every_later_check_pay_for_it() {
        // M5. An attacker posting a hundred thousand distinct usernames locks
        // out a hundred thousand buckets, all of which a `retain` keeps. The
        // sweep used to run on every check past a thousand entries, so each
        // legitimate sign-in afterwards scanned the whole attack under the
        // mutex, on a reactor thread.
        let limiter = limiter(1);

        for index in 0..(MAX_BUCKETS + 5_000) {
            limiter.record_failure(address(), &format!("victim-{index}"));
        }

        assert!(
            limiter.tracked() <= MAX_BUCKETS,
            "{} buckets is past the ceiling",
            limiter.tracked()
        );

        let started = std::time::Instant::now();

        for index in 0..10_000 {
            let _ = limiter.check(address(), &format!("ada-{index}"));
        }

        let elapsed = started.elapsed();

        // Generous by two orders of magnitude: the same loop against a full map
        // and a sweep per check is a billion comparisons, which is minutes.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "ten thousand checks took {elapsed:?} against a full map"
        );
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
