//! Failures counted twice, so that guessing costs time and flooding costs it
//! more.
//!
//! Every endpoint that accepts a secret — a passkey assertion, a refresh token,
//! a setup token, a password — goes through here first. Each failure is
//! counted in two tiers:
//!
//! * **tier 1, the address** — IPv4 at /32, IPv6 at /64 and again at /48 —
//!   whatever it was guessing at. Three hundred failures a minute from one
//!   address (three thousand from one /48) lock everything from it out for
//!   fifteen minutes. The bar is high so that one address fronting many users
//!   — CloudTAK's server, an office NAT — is not locked out by a few of them;
//!   what it stops is one address guessing without end, and it caps what a
//!   single address can do to the tier below: about thirty pairs a window at
//!   the defaults;
//! * **tier 2, the pair** — that address, cut the same way, and the subject:
//!   a username, a client, an endpoint's own name. Ten failures a minute lock
//!   the pair out for fifteen. Keying it on the username alone would let
//!   anybody who knows an account's name lock it out from anywhere — CloudTAK's
//!   OAuth client and its password-grant account above all — so the address
//!   stays in the key, at the accepted cost that a guess spread across many
//!   addresses gets an allowance per address.
//!
//! A caller is refused while either tier has it locked out.
//!
//! # Why there is no map
//!
//! This used to be a `Mutex<HashMap<(address, subject), Bucket>>`, and each of
//! its weaknesses was a way to deny sign-in to everybody: every check took the
//! one lock; at its ceiling every new key scanned the whole map under it; its
//! memory grew with attacker-chosen strings; and a flood that filled it evicted
//! — forgave — the lockouts already in it.
//!
//! Each tier is now a count-min sketch (`ratelimit::sketch`): a fixed array of
//! atomic cells, allocated once, in which a key is a handful of cells chosen by
//! a keyed hash and its count is the least of theirs. Nothing is stored per key,
//! so there is nothing to grow, evict or sweep; a check is a few atomic loads,
//! a failure a few compare-and-swaps, and no request path allocates, locks or
//! loops over a table. A key is refused only when *every* one of its cells is
//! locked, after Stochastic Fair Blue, so an innocent key that shares some
//! cells with an attacker's is still let through.
//!
//! The price is that counts are estimates: never below the truth, sometimes
//! above it when keys share cells. Under a flood large enough to lock a
//! sizeable share of the cells, a key that never failed is refused with
//! probability about `(1 − e^(−K/WIDTH))^DEPTH` for `K` lockouts in force —
//! the limiter fails closed, and the console shows how full each tier is.
//!
//! # A success forgives nothing
//!
//! A sketch cannot take one key's failures away without taking them from every
//! key sharing its cells too, which would undercount them. So the rule is ten
//! failures in a window, whether or not a success came between, and
//! [`RateLimiter::record_success`] is kept only so that its callers say what
//! happened.
//!
//! # Why in memory
//!
//! rustak is a single process, and a restart forgiving every lockout early is
//! the only thing persistence would buy. The sketch is 4 MiB whatever happens.
//!
//! # What an administrator can see
//!
//! A sketch cannot list its keys, so each lockout is also recorded as an event
//! in a small ring (`ratelimit::events`), never consulted for a decision. `view`
//! lists the events whose keys the sketch still refuses and clears one key's
//! cells; each refusal and each lockout is counted per [`class`].

pub mod class;
mod clock;
mod events;
mod key;
mod sketch;
mod view;

use std::fmt;
use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use rustak_api::LockoutClass;

use crate::config::RateLimitConfig;

use self::clock::Clock;
use self::events::{Counted, Event, Ring, Tier};
use self::key::{KeyedHash, ShownKey, Source};
use self::sketch::{Table, WIDTH};

pub use class::{CLIENT_PREFIX, classify, subject_for, subjects};
pub use view::Unclearable;

/// The allowance of each kind of key, as the 16-bit cells can count it.
#[derive(Debug, Clone, Copy)]
struct Limits {
    host: u16,
    network: u16,
    pair: u16,
}

/// Refuses a caller that has been failing.
pub struct RateLimiter {
    keyed: KeyedHash,
    sources: Table,
    pairs: Table,
    clock: Clock,
    limits: Limits,
    lockout: Duration,
    ring: Ring,
    counters: class::Counters,
    /// When this limiter started counting, which is when the process did.
    created_at: DateTime<Utc>,
}

impl fmt::Debug for RateLimiter {
    /// Written out so the hash key and four megabytes of cells stay out of it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RateLimiter")
            .field("limits", &self.limits)
            .field("lockout", &self.lockout)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

impl RateLimiter {
    /// Builds a limiter from `[auth.rate_limit]`, with a fresh hash key.
    pub fn new(config: &RateLimitConfig) -> Self {
        Self::build(config, WIDTH, KeyedHash::random(), Utc::now())
    }

    fn build(config: &RateLimitConfig, width: usize, keyed: KeyedHash, now: DateTime<Utc>) -> Self {
        let allowance = |attempts: u32| u16::try_from(attempts).unwrap_or(u16::MAX).max(1);

        Self {
            keyed,
            sources: Table::new(width),
            pairs: Table::new(width),
            clock: Clock::new(now, config.window, config.lockout),
            limits: Limits {
                host: allowance(config.address_attempts),
                network: allowance(config.network_attempts),
                pair: allowance(config.attempts),
            },
            lockout: config.lockout,
            ring: Ring::new(),
            counters: class::Counters::default(),
            created_at: now,
        }
    }

    /// Whether this caller may attempt now, and for how long it may not.
    ///
    /// The subject is whatever the endpoint is guessing at — a username, a
    /// client, the endpoint's own name — so that failures against one account
    /// do not lock out another from the same office, until the office as a
    /// whole has failed too often.
    pub fn check(&self, client_ip: Option<IpAddr>, subject: &str) -> Result<(), Duration> {
        self.check_at(Utc::now(), client_ip, subject)
    }

    /// [`check`](Self::check), at a moment the caller chooses.
    fn check_at(
        &self,
        now: DateTime<Utc>,
        client_ip: Option<IpAddr>,
        subject: &str,
    ) -> Result<(), Duration> {
        let stamp = self.clock.stamp(now);
        let host = Source::host(client_ip);

        let source = [Some(host), Source::network(client_ip)]
            .into_iter()
            .flatten()
            .filter_map(|source| self.sources.locked_until(self.keyed.source(&source), stamp))
            .max();
        let pair = self
            .pairs
            .locked_until(self.keyed.pair(&host, subject), stamp);

        let Some(until) = source.max(pair) else {
            return Ok(());
        };

        self.counters.refused(match source {
            Some(_) => LockoutClass::Source,
            None => classify(ShownKey::of(subject).as_str()).0,
        });

        Err(self.clock.at(until) - now)
    }

    /// Counts a failure, locking out whichever key it takes over its limit.
    ///
    /// Returns the lockout when this failure started one, in either tier, so
    /// the caller can log or audit the moment it starts rather than
    /// discovering it on the next attempt.
    pub fn record_failure(&self, client_ip: Option<IpAddr>, subject: &str) -> Option<Duration> {
        self.record_failures_at(Utc::now(), client_ip, &[subject])
    }

    /// One failed attempt that was guessing at several subjects at once — a
    /// workload identity and the account it named: each pair is counted, the
    /// address once.
    pub fn record_failures(
        &self,
        client_ip: Option<IpAddr>,
        subjects: &[&str],
    ) -> Option<Duration> {
        self.record_failures_at(Utc::now(), client_ip, subjects)
    }

    /// [`record_failures`](Self::record_failures), at a moment the caller
    /// chooses.
    fn record_failures_at(
        &self,
        now: DateTime<Utc>,
        client_ip: Option<IpAddr>,
        subjects: &[&str],
    ) -> Option<Duration> {
        let host = Source::host(client_ip);
        let mut began = false;

        for (source, allowance) in [
            (Some(host), self.limits.host),
            (Source::network(client_ip), self.limits.network),
        ] {
            if let Some(source) = source {
                began |= self.count(self.source_key(source), allowance, now);
            }
        }

        for subject in subjects {
            began |= self.count(self.pair_key(host, subject), self.limits.pair, now);
        }

        began.then_some(self.lockout)
    }

    /// Kept so that every endpoint still says when a credential worked; it
    /// forgives nothing. See "A success forgives nothing" above.
    pub fn record_success(&self, _client_ip: Option<IpAddr>, _subject: &str) {}

    /// The bytes this limiter occupies, which never changes after it is built.
    pub fn footprint(&self) -> usize {
        std::mem::size_of::<Self>() + self.sources.bytes() + self.pairs.bytes() + self.ring.bytes()
    }

    /// Counts one failure of `key`; whether it started a lockout.
    fn count(&self, key: Counted, allowance: u16, now: DateTime<Utc>) -> bool {
        let stamp = self.clock.stamp(now);
        let recorded =
            self.table(key.tier)
                .record(key.hash, stamp, allowance, self.clock.lockout_ends(stamp));

        if recorded.began.is_none() {
            return false;
        }

        self.counters.locked(key.class);
        self.ring.push(Event::new(key, now, recorded.estimate));

        true
    }

    /// A tier-1 key.
    fn source_key(&self, source: Source) -> Counted {
        Counted {
            tier: Tier::Source,
            hash: self.keyed.source(&source),
            class: LockoutClass::Source,
            source,
            subject: None,
        }
    }

    /// A tier-2 key.
    fn pair_key(&self, host: Source, subject: &str) -> Counted {
        let shown = ShownKey::of(subject);

        Counted {
            tier: Tier::Pair,
            hash: self.keyed.pair(&host, subject),
            class: classify(shown.as_str()).0,
            source: host,
            subject: Some(shown),
        }
    }

    fn table(&self, tier: Tier) -> &Table {
        match tier {
            Tier::Source => &self.sources,
            Tier::Pair => &self.pairs,
        }
    }
}

#[cfg(test)]
mod simulation_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter_with(config: RateLimitConfig) -> (RateLimiter, DateTime<Utc>) {
        let at = Utc::now();

        (
            RateLimiter::build(&config, 1 << 12, KeyedHash::fixed(3, 5), at),
            at,
        )
    }

    /// Every allowance explicit, so a change of default moves none of these
    /// tests; tier 1 well above anything a test here reaches by accident.
    fn limiter(attempts: u32) -> (RateLimiter, DateTime<Utc>) {
        limiter_with(RateLimitConfig {
            attempts,
            address_attempts: 20,
            network_attempts: 60,
            ..RateLimitConfig::default()
        })
    }

    fn ip(text: &str) -> Option<IpAddr> {
        Some(text.parse().unwrap())
    }

    fn address() -> Option<IpAddr> {
        ip("198.51.100.4")
    }

    fn lockouts_of(limiter: &RateLimiter, class: LockoutClass) -> u64 {
        limiter.counters.snapshot()[class.index()].lockouts
    }

    #[test]
    fn a_key_with_no_history_is_let_through() {
        assert!(
            RateLimiter::new(&RateLimitConfig::default())
                .check(address(), "ada")
                .is_ok()
        );
    }

    #[test]
    fn the_lockout_starts_on_the_attempt_that_crosses_the_limit() {
        let (limiter, at) = limiter(3);
        let fail = || limiter.record_failures_at(at, address(), &["ada"]);

        assert_eq!(fail(), None);
        assert_eq!(fail(), None);
        assert!(limiter.check_at(at, address(), "ada").is_ok());
        assert_eq!(fail(), Some(Duration::minutes(15)));

        let remaining = limiter.check_at(at, address(), "ada").unwrap_err();
        assert!(remaining <= Duration::minutes(15) && remaining > Duration::minutes(14));
    }

    #[test]
    fn a_lockout_is_reported_once_rather_than_extended_by_every_later_attempt() {
        // Otherwise an attacker who keeps hammering would keep resetting their
        // own lockout, and every caller would log or audit it per attempt.
        let (limiter, at) = limiter(1);

        assert!(
            limiter
                .record_failures_at(at, address(), &["ada"])
                .is_some()
        );
        let ends = limiter.check_at(at, address(), "ada").unwrap_err();

        let later = at + Duration::minutes(5);
        assert_eq!(limiter.record_failures_at(later, address(), &["ada"]), None);
        assert_eq!(
            limiter.check_at(later, address(), "ada").unwrap_err(),
            ends - Duration::minutes(5)
        );
    }

    #[test]
    fn a_lockout_runs_out_on_its_own() {
        let (limiter, at) = limiter(1);

        limiter.record_failures_at(at, address(), &["ada"]);

        assert!(
            limiter
                .check_at(at + Duration::seconds(899), address(), "ada")
                .is_err()
        );
        assert!(
            limiter
                .check_at(at + Duration::seconds(900), address(), "ada")
                .is_ok()
        );
    }

    #[test]
    fn failures_against_one_account_do_not_lock_out_another() {
        let (limiter, at) = limiter(1);

        limiter.record_failures_at(at, address(), &["ada"]);

        assert!(limiter.check_at(at, address(), "grace").is_ok());
        assert!(limiter.check_at(at, ip("198.51.100.5"), "ada").is_ok());
    }

    #[test]
    fn a_missing_address_is_a_key_of_its_own() {
        let (limiter, at) = limiter(1);

        limiter.record_failures_at(at, None, &["ada"]);

        assert!(limiter.check_at(at, None, "ada").is_err());
        assert!(limiter.check_at(at, ip("0.0.0.0"), "ada").is_ok());
        assert!(limiter.check_at(at, address(), "ada").is_ok());
    }

    #[test]
    fn a_success_no_longer_forgives_the_failures_before_it() {
        // A sketch cannot take one key's failures away without taking them
        // from its neighbours too. The rule is now three (here) failures in a
        // window, whether or not a success came between.
        let (limiter, at) = limiter(3);

        limiter.record_failures_at(at, address(), &["ada"]);
        limiter.record_failures_at(at, address(), &["ada"]);
        limiter.record_success(address(), "ada");

        assert!(
            limiter
                .record_failures_at(at, address(), &["ada"])
                .is_some()
        );
    }

    #[test]
    fn a_window_that_has_run_out_starts_counting_again() {
        // The limit is ten failures a minute, not ten failures ever: somebody
        // who mistypes a token twice a day should never be locked out.
        let (limiter, at) = limiter(2);

        assert_eq!(limiter.record_failures_at(at, address(), &["ada"]), None);

        let later = at + Duration::minutes(2);
        assert_eq!(
            limiter.record_failures_at(later, address(), &["ada"]),
            None,
            "a failure in a new window is the first one, not the second",
        );
        assert!(limiter.check_at(later, address(), "ada").is_ok());
    }

    #[test]
    fn an_account_name_is_counted_however_it_is_cased() {
        let (limiter, at) = limiter(3);

        for spelling in ["Ada", " ADA", "ada "] {
            limiter.record_failures_at(at, address(), &[spelling]);
        }

        assert!(limiter.check_at(at, address(), "aDa").is_err());
    }

    #[test]
    fn an_ipv4_mapped_address_is_the_ipv4_address() {
        let (limiter, at) = limiter(2);

        limiter.record_failures_at(at, ip("::ffff:198.51.100.4"), &["ada"]);
        limiter.record_failures_at(at, address(), &["ada"]);

        assert!(limiter.check_at(at, address(), "ada").is_err());
        assert!(
            limiter
                .check_at(at, ip("::ffff:198.51.100.4"), "ada")
                .is_err()
        );
    }

    #[test]
    fn one_address_trying_many_usernames_runs_out_at_tier_one() {
        // A caller that checks first, as every endpoint does, and moves on to
        // the next username when one is refused: ten guesses at each. At the
        // shipped allowances (300 an address, 10 a pair, written out here so
        // the figure is this test's own), that is thirty accounts locked out
        // before the address is.
        let (limiter, at) = limiter_with(RateLimitConfig {
            attempts: 10,
            address_attempts: 300,
            network_attempts: 3_000,
            ..RateLimitConfig::default()
        });
        let mut failures = 0;

        'guessing: for user in 0..1_000 {
            for _ in 0..10 {
                let username = format!("user-{user}");
                if limiter.check_at(at, address(), &username).is_err() {
                    if limiter.check_at(at, address(), "never-tried").is_err() {
                        break 'guessing;
                    }
                    continue 'guessing;
                }
                limiter.record_failures_at(at, address(), &[&username]);
                failures += 1;
            }
        }

        assert_eq!(failures, 300, "the address's whole allowance, and no more");
        assert_eq!(lockouts_of(&limiter, LockoutClass::Source), 1);
        assert!(
            lockouts_of(&limiter, LockoutClass::Account) <= 30,
            "at most allowance / attempts accounts locked",
        );
        assert!(limiter.check_at(at, ip("198.51.100.5"), "user-0").is_ok());
    }

    #[test]
    fn an_ipv6_64_is_one_address_and_a_48_is_counted_too() {
        let (limiter, at) = limiter_with(RateLimitConfig {
            attempts: 10,
            address_attempts: 4,
            network_attempts: 12,
            ..RateLimitConfig::default()
        });
        let in_64 = |host: u32| ip(&format!("2001:db8:1:2::{host:x}"));
        let in_48 = |subnet: u32, host: u32| ip(&format!("2001:db8:1:{subnet:x}::{host:x}"));

        for host in 0..4 {
            limiter.record_failures_at(at, in_64(host), &[&format!("guess-{host}")]);
        }
        assert!(limiter.check_at(at, in_64(0xbeef), "anybody").is_err());
        assert!(limiter.check_at(at, in_48(3, 1), "anybody").is_ok());

        // Twelve failures in the /48 in all: eight more across three /64s,
        // none of them reaching four.
        let mut failures = 0;
        'spread: for subnet in 0x10..0x13 {
            for host in 0..3 {
                limiter.record_failures_at(
                    at,
                    in_48(subnet, host),
                    &[&format!("g-{subnet}-{host}")],
                );
                failures += 1;
                if failures == 8 {
                    break 'spread;
                }
            }
        }
        assert!(limiter.check_at(at, in_48(0xffff, 1), "anybody").is_err());
        assert!(limiter.check_at(at, ip("2001:db8:2::1"), "anybody").is_ok());
        assert_eq!(lockouts_of(&limiter, LockoutClass::Source), 2);
    }

    #[test]
    fn one_attempt_at_several_subjects_counts_the_address_once() {
        let (limiter, at) = limiter_with(RateLimitConfig {
            attempts: 100,
            address_attempts: 4,
            ..RateLimitConfig::default()
        });

        for _ in 0..3 {
            limiter.record_failures_at(at, address(), &["workload-identity", "ada"]);
        }
        assert!(limiter.check_at(at, address(), "anybody").is_ok());

        limiter.record_failures_at(at, address(), &["workload-identity", "ada"]);
        assert!(limiter.check_at(at, address(), "anybody").is_err());
    }

    #[test]
    fn an_unknown_account_is_counted_exactly_like_a_real_one() {
        // The limiter never asks whether an account exists — that would make
        // its answer an oracle — so a name nobody holds locks the same way.
        let (limiter, at) = limiter(3);

        for subject in ["ada", "nobody-has-this-name"] {
            let began: Vec<_> = (0..3)
                .map(|_| {
                    limiter
                        .record_failures_at(at, address(), &[subject])
                        .is_some()
                })
                .collect();

            assert_eq!(began, [false, false, true], "{subject}");
        }
    }

    #[test]
    fn refusals_and_lockouts_are_counted_by_class_and_never_by_key() {
        let (limiter, at) = limiter(1);

        limiter.record_failures_at(at, address(), &["ada"]);
        let _ = limiter.check_at(at, address(), "ada");
        let _ = limiter.check_at(at, address(), "Ada");
        limiter.record_failures_at(at, address(), &[subjects::PASSKEY]);
        let _ = limiter.check_at(at, address(), "grace");

        let counters = limiter.counters.snapshot();
        let of = |class: LockoutClass| counters[class.index()];

        assert_eq!(of(LockoutClass::Account).lockouts, 1);
        assert_eq!(of(LockoutClass::Account).refusals, 2);
        assert_eq!(of(LockoutClass::Address).lockouts, 1);
        assert_eq!(of(LockoutClass::Address).refusals, 0);
        assert_eq!(of(LockoutClass::Client).lockouts, 0);
        assert_eq!(of(LockoutClass::Source).lockouts, 0);
    }

    #[test]
    fn the_hash_key_stays_out_of_a_debug_dump() {
        let (limiter, _) = limiter(1);
        let rendered = format!("{limiter:?}");

        assert!(rendered.starts_with("RateLimiter"), "{rendered}");
        assert!(
            !rendered.contains("k0") && !rendered.contains("keyed"),
            "{rendered}"
        );
    }
}
