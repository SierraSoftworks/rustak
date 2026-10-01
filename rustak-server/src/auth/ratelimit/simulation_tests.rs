//! The limiter's statistical and concurrent properties, measured.
//!
//! Everything here runs at injected instants with a fixed hash key, so every
//! number is the same on every run and every host; nothing is timed.

use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::clock::Clock;
use super::sketch::{DEPTH, WIDTH};
use super::*;

fn config(attempts: u32) -> RateLimitConfig {
    RateLimitConfig {
        attempts,
        address_attempts: u32::from(u16::MAX),
        network_attempts: u32::from(u16::MAX),
        ..RateLimitConfig::default()
    }
}

fn limiter(config: &RateLimitConfig, width: usize) -> (RateLimiter, DateTime<Utc>) {
    let at = Utc::now();

    (
        RateLimiter::build(config, width, KeyedHash::fixed(0x5EED, 0xC0FFEE), at),
        at,
    )
}

/// A distinct address for every index, so tier 1 never joins in.
fn address(index: u32) -> Option<IpAddr> {
    let [a, b, c, d] = index.to_be_bytes();

    Some(IpAddr::from([10 | (a & 0x0F), b, c, d]))
}

/// The pair's estimate, as the sketch has it.
fn estimate(
    limiter: &RateLimiter,
    at: DateTime<Utc>,
    client: Option<IpAddr>,
    subject: &str,
) -> u16 {
    let stamp = limiter.clock.stamp(at);

    limiter.pairs.estimate(
        limiter.keyed.pair(&Source::host(client), subject),
        stamp.window,
    )
}

/// How false refusals grow with the number of lockouts in force.
///
/// Two production-sized sketches side by side, all inside one window, keys
/// hashed exactly as the limiter hashes an address. In the first, `K` keys
/// fail ten times and are locked; 50 000 keys that never failed are then
/// asked whether they are refused. In the second, `K` keys fail nine times —
/// one short, so nothing is locked — and 50 000 strangers are asked whether a
/// *single* failure would lock them. Both are compared with
/// `(1 − e^(−K/WIDTH))^DEPTH`: a stranger is caught only when every one of its
/// cells was taken by somebody else. The probes only read. Run with
/// `--nocapture` for the table.
#[test]
fn false_refusals_are_rare_and_as_rare_as_the_arithmetic_says() {
    const PROBES: u32 = 50_000;
    const ALLOWANCE: u16 = 10;

    let clock = Clock::new(Utc::now(), Duration::minutes(1), Duration::minutes(15));
    let now = clock.stamp(Utc::now());
    let until = clock.lockout_ends(now);
    let (locked, noisy) = (Table::new(WIDTH), Table::new(WIDTH));
    // A hash key each, so the two columns are two experiments, not one twice.
    let (keyed, other) = (
        KeyedHash::fixed(0x5EED, 0xC0FFEE),
        KeyedHash::fixed(0xD1CE, 0xFACE),
    );
    let key = |keyed: &KeyedHash, index: u32| keyed.source(&Source::host(address(index)));

    let checkpoints = [100, 1_000, WIDTH / 4, WIDTH / 2, WIDTH, 2 * WIDTH];
    let mut keys = 0u32;

    eprintln!(
        "| K | K / WIDTH | (1 − e^(−K/W))^D | refused, K locked | locked by one failure, K at nine |"
    );
    eprintln!("|---|---|---|---|---|");

    for target in checkpoints {
        while (keys as usize) < target {
            let (one, two) = (key(&keyed, keys), key(&other, keys));
            for failure in 0..ALLOWANCE {
                locked.record(one, now, ALLOWANCE, until);
                if failure + 1 < ALLOWANCE {
                    noisy.record(two, now, ALLOWANCE, until);
                }
            }
            keys += 1;
        }

        let (mut refused, mut caught) = (0u32, 0u32);
        for probe in 0..PROBES {
            // Addresses beyond every one that failed.
            let stranger = 0x00FF_FFFF - probe;

            refused += u32::from(locked.locked_until(key(&keyed, stranger), now).is_some());
            caught += u32::from(noisy.estimate(key(&other, stranger), now.window) + 1 >= ALLOWANCE);
        }

        let ratio = target as f64 / WIDTH as f64;
        let expected = (1.0 - (-ratio).exp()).powi(DEPTH as i32);
        let rate = |count: u32| f64::from(count) / f64::from(PROBES);

        eprintln!(
            "| {target} | {ratio:.4} | {expected:.6} | {:.6} ({refused}) | {:.6} ({caught}) |",
            rate(refused),
            rate(caught),
        );

        for (what, count) in [("refused", refused), ("caught", caught)] {
            if target <= 1_000 {
                assert_eq!(count, 0, "{target} keys: {count} strangers {what}");
                continue;
            }

            // Three standard deviations of a binomial, and a tenth for the
            // approximation itself.
            let sigma = (expected * (1.0 - expected) / f64::from(PROBES)).sqrt();
            assert!(
                (rate(count) - expected).abs() <= 3.0 * sigma + 0.1 * expected,
                "{target} keys: {what} {}, expected {expected}",
                rate(count),
            );
        }
    }
}

/// Never undercounts, single-threaded: on a table so narrow that every key
/// shares cells, every key's estimate is at least its own failures.
#[test]
fn no_key_is_ever_counted_below_its_own_failures() {
    let (limiter, at) = limiter(&config(u32::from(u16::MAX)), 64);
    let mut truth = Vec::new();

    for key in 0..2_000u32 {
        // A spread of counts, the same on every run.
        let failures = (key.wrapping_mul(2_654_435_761) >> 28) % 12;
        for _ in 0..failures {
            limiter.record_failures_at(at, address(key), &["ada"]);
        }
        truth.push((key, failures));
    }

    let mut over = 0;
    for (key, failures) in truth {
        let counted = u32::from(estimate(&limiter, at, address(key), "ada"));

        assert!(counted >= failures, "key {key}: {counted} < {failures}");
        over += u32::from(counted > failures);
    }

    assert!(over > 0, "a table this narrow must overcount somewhere");
}

/// Never undercounts, concurrently: sixteen threads hammer one key while four
/// more hammer others that share its cells. With conservative update and
/// compare-and-swap, the hammered key counts every failure, exactly when
/// nothing else touches it and at least when something does.
#[test]
fn failures_from_many_threads_at_once_are_all_counted() {
    const THREADS: u32 = 16;
    const EACH: u32 = 4_000;

    for neighbours in [0u32, 4] {
        let (limiter, at) = limiter(&config(u32::from(u16::MAX)), 64);
        let limiter = Arc::new(limiter);
        let start = Arc::new(std::sync::Barrier::new((THREADS + neighbours) as usize));

        let handles: Vec<_> = (0..THREADS + neighbours)
            .map(|thread| {
                let (limiter, start) = (limiter.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    for attempt in 0..EACH {
                        let (client, subject) = match thread < THREADS {
                            true => (address(1), "ada".to_string()),
                            false => (address(100 + thread), format!("noise-{}", attempt % 50)),
                        };
                        limiter.record_failures_at(at, client, &[&subject]);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let counted = u32::from(estimate(&limiter, at, address(1), "ada"));
        let host = limiter.keyed.source(&Source::host(address(1)));
        let address_count = u32::from(
            limiter
                .sources
                .estimate(host, limiter.clock.stamp(at).window),
        );

        assert!(
            counted >= THREADS * EACH,
            "{neighbours} neighbours: {counted}"
        );
        assert!(address_count >= THREADS * EACH, "{address_count}");
        if neighbours == 0 {
            assert_eq!(counted, THREADS * EACH, "alone, the count is exact");
            assert_eq!(address_count, THREADS * EACH);
        }
    }
}

/// Memory is fixed: a million distinct failing keys later, the limiter is the
/// size it was when it was built.
#[test]
fn a_million_failing_keys_cost_nothing_more_than_none() {
    let (limiter, at) = limiter(&RateLimitConfig::default(), WIDTH);
    let empty = limiter.footprint();

    // A million addresses, each a new key in both tiers.
    for key in 0..1_000_000u32 {
        limiter.record_failures_at(at, address(key), &["ada"]);
    }

    assert_eq!(limiter.footprint(), empty);
    assert!(
        empty < 5 * 1024 * 1024,
        "two tiers of 4 × 2¹⁶ cells and the ring: {empty} bytes",
    );
}
