//! A short-lived memory of secrets that have already been verified.
//!
//! argon2id costs 19 MiB and tens of milliseconds by design, and the clients
//! that present a secret rather than a certificate present it on *every*
//! request: CloudTAK re-sends Basic credentials with each `signClient` call, and
//! a reconnecting stream client re-sends its `<auth>` block. Paying a full hash
//! for each of those turns a busy listener into a memory-bound queue.
//!
//! So a verification that succeeded is remembered for five minutes, keyed on the
//! credential's row id *and* the secret that was offered. The key is a sha256 of
//! both, so nothing here is a secret or reversible into one, and a wrong secret
//! can never hit an entry a right one put there.
//!
//! # What it deliberately does not do
//!
//! It does not cache failures — a wrong secret costs a full hash every time,
//! which is what makes guessing expensive. It does not outlive the process, so a
//! restart re-verifies everything. And it is cleared per credential by
//! [`VerifiedSecretCache::forget`], which every revocation calls, so a revoked
//! credential stops working now rather than in five minutes.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use rustak_core::prelude::*;
use sha2::{Digest as _, Sha256};

/// How long a verified secret is remembered.
pub const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// How many entries are kept before the oldest are dropped.
///
/// Reached only by an installation with thousands of live credentials, and a
/// miss costs one argon2 verification rather than a failure, so the cap is a
/// memory bound rather than a policy.
pub const DEFAULT_CAPACITY: usize = 4096;

/// The process-wide cache every listener shares.
static SHARED: LazyLock<VerifiedSecretCache> = LazyLock::new(VerifiedSecretCache::default);

/// What a hit or a miss is keyed on: sha256(credential id ‖ secret).
type Key = [u8; 32];

/// Secrets that verified recently, so the next request does not re-hash them.
#[derive(Debug)]
pub struct VerifiedSecretCache {
    /// Poisoning is impossible: nothing here panics while the lock is held.
    entries: Mutex<HashMap<Key, (CredentialId, Instant)>>,
    ttl: Duration,
    capacity: usize,
}

impl Default for VerifiedSecretCache {
    fn default() -> Self {
        Self::new(DEFAULT_TTL, DEFAULT_CAPACITY)
    }
}

impl VerifiedSecretCache {
    /// A cache with a lifetime and a cap of its own, for tests.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            capacity,
        }
    }

    /// The cache the whole process shares.
    ///
    /// A single instance rather than one per listener, because the same
    /// credential is presented to the Marti listener, the stream listener and
    /// `/oauth/token`, and each of those paying its own first hash for the same
    /// secret is three times the cost for no benefit.
    pub fn shared() -> &'static Self {
        &SHARED
    }

    /// Whether this exact secret verified against this credential recently.
    pub fn contains(&self, credential: CredentialId, secret: &str) -> bool {
        let key = key(credential, secret);
        let now = Instant::now();
        let mut entries = self.lock();

        match entries.get(&key) {
            Some((_, seen)) if now.duration_since(*seen) < self.ttl => true,
            Some(_) => {
                entries.remove(&key);
                false
            }
            None => false,
        }
    }

    /// Remembers a verification that succeeded.
    pub fn remember(&self, credential: CredentialId, secret: &str) {
        let mut entries = self.lock();

        if entries.len() >= self.capacity {
            let cutoff = Instant::now() - self.ttl;
            entries.retain(|_, (_, seen)| *seen > cutoff);

            // Still full: the installation has more live credentials than the
            // cap, so the cache stops taking new ones rather than growing.
            if entries.len() >= self.capacity {
                return;
            }
        }

        entries.insert(key(credential, secret), (credential, Instant::now()));
    }

    /// Forgets everything remembered about one credential.
    ///
    /// Called by every revocation: without it a credential taken back would go
    /// on working for whatever is left of its entry's lifetime, which is the
    /// one thing a revocation exists to prevent.
    pub fn forget(&self, credential: CredentialId) {
        self.lock().retain(|_, (id, _)| *id != credential);
    }

    /// Drops entries that have expired, for the periodic sweep.
    pub fn sweep(&self) {
        let cutoff = Instant::now() - self.ttl;

        self.lock().retain(|_, (_, seen)| *seen > cutoff);
    }

    /// How many entries are held, for the sweep's metrics and for tests.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The map, recovering from a poisoned lock.
    ///
    /// A cache is not state anything depends on being correct — a lost entry is
    /// one extra hash — so a panic elsewhere must not take the login path down
    /// with it.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, (CredentialId, Instant)>> {
        self.entries.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Binds the secret to the credential it was offered for.
///
/// Including the id means a secret that happens to be right for one credential
/// cannot produce a hit for another, which matters because two users may quite
/// legitimately choose the same client password.
fn key(credential: CredentialId, secret: &str) -> Key {
    let mut hasher = Sha256::new();
    hasher.update(credential.get().to_be_bytes());
    hasher.update(b"\0");
    hasher.update(secret.as_bytes());

    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> VerifiedSecretCache {
        VerifiedSecretCache::new(DEFAULT_TTL, DEFAULT_CAPACITY)
    }

    #[test]
    fn a_secret_that_verified_is_remembered_and_a_different_one_is_not() {
        let cache = cache();
        let credential = CredentialId::new(1);

        assert!(!cache.contains(credential, "s3cret"));

        cache.remember(credential, "s3cret");

        assert!(cache.contains(credential, "s3cret"));
        assert!(
            !cache.contains(credential, "s3cre"),
            "a near miss must cost a full hash",
        );
    }

    #[test]
    fn the_same_secret_on_another_credential_is_a_miss() {
        // Two people may quite reasonably pick the same client password; one
        // of them verifying must not sign the other in.
        let cache = cache();
        cache.remember(CredentialId::new(1), "s3cret");

        assert!(!cache.contains(CredentialId::new(2), "s3cret"));
    }

    #[test]
    fn an_entry_that_has_aged_out_is_a_miss_and_is_dropped() {
        let cache = VerifiedSecretCache::new(Duration::ZERO, DEFAULT_CAPACITY);
        cache.remember(CredentialId::new(1), "s3cret");

        assert!(!cache.contains(CredentialId::new(1), "s3cret"));
        assert!(cache.is_empty(), "the miss should take the entry with it");
    }

    #[test]
    fn revoking_a_credential_forgets_every_secret_offered_for_it() {
        // The whole point: a revocation has to take effect now, not when the
        // entry would have expired anyway.
        let cache = cache();
        cache.remember(CredentialId::new(1), "first");
        cache.remember(CredentialId::new(1), "second");
        cache.remember(CredentialId::new(2), "other");

        cache.forget(CredentialId::new(1));

        assert!(!cache.contains(CredentialId::new(1), "first"));
        assert!(!cache.contains(CredentialId::new(1), "second"));
        assert!(
            cache.contains(CredentialId::new(2), "other"),
            "another credential's entry is nothing to do with this revocation",
        );
    }

    #[test]
    fn the_sweep_drops_what_has_expired_and_keeps_what_has_not() {
        let cache = cache();
        cache.remember(CredentialId::new(1), "s3cret");

        cache.sweep();
        assert_eq!(cache.len(), 1);

        let expired = VerifiedSecretCache::new(Duration::ZERO, DEFAULT_CAPACITY);
        expired.remember(CredentialId::new(1), "s3cret");
        expired.sweep();

        assert!(expired.is_empty());
    }

    #[test]
    fn a_full_cache_stops_taking_entries_rather_than_growing() {
        let cache = VerifiedSecretCache::new(DEFAULT_TTL, 2);

        for index in 0..5 {
            cache.remember(CredentialId::new(index), "s3cret");
        }

        assert_eq!(cache.len(), 2);
        assert!(
            cache.contains(CredentialId::new(0), "s3cret"),
            "a miss is one extra hash, so what is already held stays",
        );
    }

    #[test]
    fn nothing_stored_is_the_secret_or_reversible_into_one() {
        let cache = cache();
        cache.remember(CredentialId::new(1), "hunter2");

        let rendered = format!("{:?}", cache.lock());

        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn the_shared_cache_is_one_instance() {
        assert!(std::ptr::eq(
            VerifiedSecretCache::shared(),
            VerifiedSecretCache::shared()
        ));
    }
}
