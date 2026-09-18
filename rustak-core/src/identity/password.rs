//! argon2id hashing for every secret rustak stores.
//!
//! Enrolment tokens, client passwords and service tokens are all verified the
//! same way and are all stored the same way: as an argon2id PHC string, at
//! [`Params::PRODUCTION`] (m = 19 MiB, t = 2, p = 1 — the RFC 9106 second
//! recommended setting). There is no configuration key for this, because the
//! cost of a password hash is a security decision rather than a performance
//! knob, and a deployment that tuned it down would have no way to know it had.
//!
//! # The one exception, and why it cannot reach a deployment
//!
//! A test suite hashes thousands of times, and 19 MiB of memory-hard work per
//! hash is minutes of a CI run that proves nothing about the algorithm.
//! `use_testing_params` switches the process to [`Params::TESTING`], and it is
//! the only way this cost ever changes. It is compiled out of a build that did
//! not ask for the `testing` feature — which is also why it is named here
//! without a link, since these documents are built without it — nothing under
//! `src/` outside the test harnesses calls it, and a stored hash records the
//! cost it was made at, so a row written cheaply stays verifiable and a
//! deployment cannot end up hashing cheaply without somebody having written
//! the call.
//!
//! # Two jobs, two functions
//!
//! Hashing is deliberately expensive, and rustak verifies credentials on paths
//! that must stay responsive — `/oauth/token`, `signClient`, the stream
//! listener's auth hook. So there are two entry points for each operation:
//! [`hash`]/[`verify`] run on the calling thread and belong in synchronous code
//! and tests, while [`hash_blocking`]/[`verify_blocking`] move the work onto
//! `tokio`'s blocking pool. **Async code must use the `_blocking` pair**: 19 MiB
//! of memory-hard work on a runtime worker stalls every other connection that
//! worker is driving, which on a busy stream listener is hundreds of EUDs.
//!
//! # Why a lookup hint exists
//!
//! A device offers a secret, not a credential id. Finding the row to verify
//! against by trying argon2 against every credential a user holds would cost one
//! full hash per row. [`lookup_hint`] is a cheap, deterministic sha256 prefix
//! stored alongside the hash: it narrows the candidates to (almost always) one
//! row, which is then verified properly. It is **not** a verifier — it is
//! unsalted and short, so a match means "try this row", never "this is the
//! secret". Sixteen hex characters is enough to distinguish the handful of live
//! credentials a user has and far too little to attack the secret through.
//!
//! # Unknown users cost the same as known ones
//!
//! [`verify_dummy`] runs a real argon2 verification against a fixed hash, so
//! that "no such user" and "wrong password" take the same time. Without it, the
//! login endpoint answers the question "does this username exist?" to anybody
//! with a stopwatch, which is the first step of every credential-stuffing run.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use argon2::{Argon2, PasswordHasher as _, PasswordVerifier as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::secret::Secret;

/// Hex characters of the sha256 prefix stored as a lookup hint.
const HINT_LENGTH: usize = 16;

/// Whether [`use_testing_params`] has been called. Only a test harness can set
/// it, and only in a build that compiled the setter in at all.
static CHEAP: AtomicBool = AtomicBool::new(false);

/// What one argon2id hash costs.
///
/// Held as plain numbers rather than as `argon2::Params` so that the two
/// settings below are readable at a glance and comparable in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Working memory, in kibibytes.
    pub memory_kib: u32,
    /// Passes over that memory.
    pub iterations: u32,
    /// Lanes.
    pub parallelism: u32,
}

impl Params {
    /// What every deployment hashes at: RFC 9106's second recommended setting.
    pub const PRODUCTION: Self = Self {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    };

    /// What a test suite hashes at, once `use_testing_params` has been called.
    ///
    /// Still a real argon2id hash with a real salt — the algorithm under test
    /// is the shipped one — but roughly five times cheaper, which is the
    /// difference between a coverage run that finishes and one that is killed.
    pub const TESTING: Self = Self {
        memory_kib: 8 * 1024,
        iterations: 1,
        parallelism: 1,
    };

    /// The cost this process hashes at.
    pub fn active() -> Self {
        if CHEAP.load(Ordering::Relaxed) {
            Self::TESTING
        } else {
            Self::PRODUCTION
        }
    }

    /// The hasher these parameters describe.
    fn hasher(self) -> Argon2<'static> {
        // Infallible: both settings above are well inside argon2's bounds, and
        // this is a private method, so no caller can reach it with numbers that
        // are not one of them.
        let params = argon2::Params::new(self.memory_kib, self.iterations, self.parallelism, None)
            .expect("our own argon2 parameters are within the algorithm's limits");

        Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
    }
}

/// Switches this process to [`Params::TESTING`] for everything hashed from now
/// on. Verification is unaffected: a hash carries the cost it was made at.
///
/// Call it from a test harness and nowhere else. See the [module
/// documentation](self).
#[cfg(any(test, feature = "testing"))]
pub fn use_testing_params() {
    CHEAP.store(true, Ordering::Relaxed);
}

/// The password [`verify_dummy`] burns time against.
///
/// Hashed once, lazily, rather than hard-coded: a checked-in PHC string would go
/// stale the day the crate's default parameters change, and a dummy that is
/// cheaper than a real verification defeats its own purpose.
static DUMMY: LazyLock<PasswordHash> = LazyLock::new(|| {
    // Infallible for a fixed, valid input: `hash` only fails when argon2
    // cannot allocate its working memory, at which point the process is not
    // going to serve a login anyway.
    hash("rustak-dummy-password").expect("hashing a fixed string cannot fail")
});

/// An argon2id hash in PHC string form, as stored in `credentials.hash`.
///
/// A hash is not a secret — it is what we keep *instead* of one — but it is
/// still material an attacker would like to have offline, so it redacts itself
/// in `Debug` output the way [`Secret`] does. [`PasswordHash::as_str`] is how it
/// reaches the database.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PasswordHash(String);

impl PasswordHash {
    /// Accepts a PHC string that is already in hand — a row read back from the
    /// database, say.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::System`] error when the text is not a
    /// PHC string we could verify against. A row we cannot parse is our bug or
    /// a corrupted database, never something the operator typed.
    pub fn parse(text: impl Into<String>) -> Result<Self, human_errors::Error> {
        let text = text.into();

        argon2::PasswordHash::new(&text).map_err(|err| {
            human_errors::system(
                format!("We could not read a stored password hash: {err}"),
                crate::errors::ADVICE_REPORT_DEV,
            )
        })?;

        Ok(Self(text))
    }

    /// The PHC string, for storage.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PasswordHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PasswordHash(***)")
    }
}

/// Hashes a secret with argon2id at [`Params::active`].
///
/// This blocks the calling thread for tens of milliseconds by design. In async
/// code use [`hash_blocking`].
///
/// # Errors
///
/// Returns a [`human_errors::Kind::System`] error if argon2 refuses the input,
/// which in practice means the process could not allocate its 19 MiB.
pub fn hash(secret: &str) -> Result<PasswordHash, human_errors::Error> {
    let hashed = Params::active()
        .hasher()
        .hash_password(secret.as_bytes())
        .map_err(|err| {
            human_errors::system(
                format!("We could not hash a credential: {err}"),
                crate::errors::ADVICE_REPORT_DEV,
            )
        })?;

    Ok(PasswordHash(hashed.to_string()))
}

/// Checks a secret against a stored hash.
///
/// Returns `false` for a wrong secret *and* for a hash we cannot parse: a
/// corrupted row must not authenticate anybody, and the caller of a verify has
/// no different action to take in the two cases. In async code use
/// [`verify_blocking`].
pub fn verify(secret: &str, hash: &PasswordHash) -> bool {
    // `Argon2::default()` here is not the cost this verification runs at: the
    // PHC string names the parameters its hash was made with, and those are the
    // ones used, which is what lets a row survive a change of [`Params`].
    Argon2::default()
        .verify_password(secret.as_bytes(), hash.as_str())
        .is_ok()
}

/// Spends the same time a real verification would, against a fixed hash.
///
/// Call this on the "no such user" and "no such credential" paths so that they
/// cannot be told apart from a wrong password by timing. Always returns `false`.
pub fn verify_dummy(secret: &str) -> bool {
    verify(secret, &DUMMY);
    false
}

/// A cheap, deterministic prefix used to find the credential row to verify.
///
/// See the [module documentation](self): this narrows candidate rows, and is
/// never on its own a reason to accept a secret.
pub fn lookup_hint(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .take(HINT_LENGTH / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// [`hash`], moved off the async runtime's worker threads.
///
/// # Errors
///
/// As [`hash`], plus a [`human_errors::Kind::System`] error if the blocking pool
/// could not run the work (which only happens during runtime shutdown).
pub async fn hash_blocking(secret: Secret) -> Result<PasswordHash, human_errors::Error> {
    tokio::task::spawn_blocking(move || hash(secret.expose()))
        .await
        .map_err(join_failed)?
}

/// [`verify`], moved off the async runtime's worker threads.
///
/// # Errors
///
/// Returns a [`human_errors::Kind::System`] error if the blocking pool could not
/// run the work. A wrong secret is `Ok(false)`, not an error.
pub async fn verify_blocking(
    secret: Secret,
    hash: PasswordHash,
) -> Result<bool, human_errors::Error> {
    tokio::task::spawn_blocking(move || verify(secret.expose(), &hash))
        .await
        .map_err(join_failed)
}

/// [`verify_dummy`], moved off the async runtime's worker threads.
///
/// # Errors
///
/// Returns a [`human_errors::Kind::System`] error if the blocking pool could not
/// run the work.
pub async fn verify_dummy_blocking(secret: Secret) -> Result<bool, human_errors::Error> {
    tokio::task::spawn_blocking(move || verify_dummy(secret.expose()))
        .await
        .map_err(join_failed)
}

/// Turns a blocking-pool failure into an error, which is always ours.
fn join_failed(err: tokio::task::JoinError) -> human_errors::Error {
    human_errors::system(
        format!("A credential hashing task could not be run: {err}"),
        crate::errors::ADVICE_REPORT_DEV,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_verifies_against_its_own_hash_and_nothing_else() {
        let hashed = hash("correct horse battery staple").unwrap();

        assert!(verify("correct horse battery staple", &hashed));
        assert!(!verify("correct horse battery stapl", &hashed));
        assert!(!verify("", &hashed));
    }

    #[test]
    fn the_same_secret_hashes_differently_every_time() {
        // A salt that did not vary would let one rainbow table cover every
        // credential in the database at once.
        let first = hash("same-secret").unwrap();
        let second = hash("same-secret").unwrap();

        assert_ne!(first.as_str(), second.as_str());
        assert!(verify("same-secret", &first) && verify("same-secret", &second));
    }

    #[test]
    fn a_stored_hash_records_which_algorithm_and_cost_produced_it() {
        // The PHC string is the reason an upgrade to stronger parameters can
        // verify yesterday's rows; a bare digest could not.
        let hashed = hash("secret").unwrap();

        assert!(hashed.as_str().starts_with("$argon2id$"), "{hashed:?}");
        assert!(hashed.as_str().contains("m=19456"), "expected RFC 9106 m");
        assert!(hashed.as_str().contains("t=2"), "expected RFC 9106 t");
    }

    #[test]
    fn a_cheaper_cost_still_produces_a_hash_the_verifier_accepts() {
        // `use_testing_params` is process-wide and these tests share a process
        // with the one above, which asserts the production cost — so this one
        // reaches for the parameters directly rather than switching them.
        assert_eq!(Params::active(), Params::PRODUCTION);
        const { assert!(Params::TESTING.memory_kib < Params::PRODUCTION.memory_kib) };

        let cheap = PasswordHash(
            Params::TESTING
                .hasher()
                .hash_password(b"secret")
                .unwrap()
                .to_string(),
        );

        // The point of the switch: a row written at one cost verifies at
        // another, because the cost travels inside the hash.
        assert!(cheap.as_str().contains("m=8192"));
        assert!(verify("secret", &cheap));
        assert!(!verify("wrong", &cheap));
    }

    #[test]
    fn a_hash_does_not_print_itself() {
        // It is not a secret, but it is offline-attackable material, so it
        // should not end up in a log line by accident.
        let hashed = hash("secret").unwrap();

        assert_eq!(format!("{hashed:?}"), "PasswordHash(***)");
    }

    #[test]
    fn a_stored_hash_can_be_read_back_from_the_database() {
        let hashed = hash("secret").unwrap();
        let round_tripped = PasswordHash::parse(hashed.as_str()).unwrap();

        assert!(verify("secret", &round_tripped));
    }

    #[test]
    fn a_corrupted_row_authenticates_nobody_rather_than_everybody() {
        // The failure mode that matters: a verifier that treated an unparseable
        // hash as a match would turn one bad row into an open door.
        assert!(PasswordHash::parse("not-a-phc-string").is_err());
        assert!(!verify(
            "anything",
            &PasswordHash("not-a-phc-string".into())
        ));
    }

    #[test]
    fn a_lookup_hint_finds_the_row_without_being_able_to_verify_it() {
        let hint = lookup_hint("a-device-password");

        assert_eq!(hint.len(), HINT_LENGTH);
        assert!(hint.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic, so the row written at mint time is the row found at
        // verify time...
        assert_eq!(hint, lookup_hint("a-device-password"));
        // ...and different secrets do not collide into one another.
        assert_ne!(hint, lookup_hint("a-device-passwore"));
    }

    #[test]
    fn a_dummy_verification_never_succeeds() {
        // It exists to spend time, and a bug that let it return `true` would be
        // an authentication bypass on the "no such user" path.
        assert!(!verify_dummy("anything at all"));
        assert!(!verify_dummy(""));
    }

    #[tokio::test]
    async fn the_blocking_pair_agrees_with_the_synchronous_one() {
        let secret = Secret::new("async-secret");
        let hashed = hash_blocking(secret.clone()).await.unwrap();

        assert!(verify(secret.expose(), &hashed));
        assert!(verify_blocking(secret, hashed.clone()).await.unwrap());
        assert!(!verify_blocking(Secret::new("wrong"), hashed).await.unwrap());
    }

    #[tokio::test]
    async fn a_dummy_verification_is_available_off_the_runtime_too() {
        // The "no such user" path is an async path, so the constant-time
        // equaliser has to be usable from it without stalling a worker.
        assert!(
            !verify_dummy_blocking(Secret::new("anything"))
                .await
                .unwrap()
        );
    }
}
