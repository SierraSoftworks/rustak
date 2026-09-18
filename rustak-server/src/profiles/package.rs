//! Building a profile package off the reactor, and only once per version.
//!
//! `GET /Marti/api/device/profile/connection` is what ATAK asks for on **every**
//! connection. Answering it reads every profile file the caller's channels
//! entitle them to into memory and deflates a zip — and both used to happen
//! synchronously on an actix worker thread, with no cache. A fleet of five
//! hundred devices reconnecting after a restart issues five hundred of those
//! within seconds; with one worker per core and a deflate that takes a couple
//! of hundred milliseconds, the listener is saturated for long enough that the
//! health check fails and the orchestrator restarts the pod, which starts the
//! whole thing again.
//!
//! Two changes. The zip is built on [`spawn_blocking`], so the reactor keeps
//! answering while it runs. And the result is kept, keyed by exactly what went
//! into it, so the five hundredth device is served the bytes the first one
//! built.
//!
//! # Why the key is not the contents
//!
//! Hashing the files would mean reading megabytes per request to decide whether
//! to avoid reading megabytes, which is no saving at all. The key is each
//! file's path, its `updated` timestamp and its length — and `updated` is
//! maintained by the only thing that can change a file, so a changed profile
//! has a different key and a cached package is never stale.
//!
//! [`spawn_blocking`]: tokio::task::spawn_blocking

use std::hash::{Hash as _, Hasher as _};
use std::sync::{LazyLock, Mutex};

use bytes::Bytes;
use rustak_core::prelude::*;

use super::builder::{ProfileFileData, build_multifile_package, build_profile_package};

/// How many built packages are kept.
///
/// An installation has a handful of profile sets — enrollment, connection, one
/// per tool — times the channel combinations its devices hold. Thirty-two
/// covers that comfortably, and the whole point is that an entry is small
/// compared with the cost of rebuilding it.
const MAX_CACHED: usize = 32;

/// What a cached package was built from.
type Key = (&'static str, String, u64);

/// The packages built so far, newest last.
///
/// A `Vec` rather than a map: at this size a linear scan is faster than
/// hashing the key, and eviction wants insertion order anyway.
static CACHE: LazyLock<Mutex<Vec<(Key, Bytes)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// One named profile's package, built once per version of it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the archive cannot be written.
pub async fn profile_package(name: &str, files: Vec<ProfileFileData>) -> Result<Bytes, Error> {
    let key = ("profile", name.to_owned(), fingerprint(&files));
    let owned = name.to_owned();

    cached_or(key, move || build_profile_package(&owned, &files)).await
}

/// The `multiFile` package a tool's several files are delivered as.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the archive cannot be written.
pub async fn multifile_package(files: Vec<ProfileFileData>) -> Result<Bytes, Error> {
    let key = ("multifile", String::new(), fingerprint(&files));

    cached_or(key, move || build_multifile_package(&files)).await
}

/// The cached bytes for `key`, or `build` run on a blocking thread.
async fn cached_or<F>(key: Key, build: F) -> Result<Bytes, Error>
where
    F: FnOnce() -> Result<Vec<u8>, Error> + Send + 'static,
{
    if let Some(hit) = lookup(&key) {
        return Ok(hit);
    }

    let built = tokio::task::spawn_blocking(build)
        .await
        .or_system_err(&["Please report this issue to the development team via GitHub."])??;

    let body = Bytes::from(built);

    store(key, body.clone());

    Ok(body)
}

/// A key that changes whenever any file in the set does.
fn fingerprint(files: &[ProfileFileData]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    files.len().hash(&mut hasher);

    for file in files {
        file.name.hash(&mut hasher);
        file.updated.timestamp_millis().hash(&mut hasher);
        file.data.len().hash(&mut hasher);
    }

    hasher.finish()
}

/// The cached package for `key`, if there is one.
fn lookup(key: &Key) -> Option<Bytes> {
    let cache = lock();

    cache
        .iter()
        .find(|(cached, _)| cached == key)
        .map(|(_, body)| body.clone())
}

/// Records a built package, evicting the oldest once the cache is full.
fn store(key: Key, body: Bytes) {
    let mut cache = lock();

    if cache.iter().any(|(cached, _)| *cached == key) {
        return;
    }

    while cache.len() >= MAX_CACHED {
        cache.remove(0);
    }

    cache.push((key, body));
}

/// The cache, recovering from a panic in another holder.
///
/// A poisoned lock here means a thread panicked while reading or writing the
/// cache. Refusing every profile download from then on would turn that into an
/// outage; the worst a recovered cache can be is a package built twice.
fn lock() -> std::sync::MutexGuard<'static, Vec<(Key, Bytes)>> {
    CACHE.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_780_000_000 + seconds, 0).unwrap()
    }

    fn files(updated: i64) -> Vec<ProfileFileData> {
        vec![ProfileFileData::new(
            "a.pref",
            b"<preferences/>".to_vec(),
            at(updated),
        )]
    }

    #[tokio::test]
    async fn the_second_device_to_connect_is_served_the_bytes_the_first_one_built() {
        let first = profile_package("cache-test-connection", files(0))
            .await
            .unwrap();
        let second = profile_package("cache-test-connection", files(0))
            .await
            .unwrap();

        assert_eq!(first, second);
        assert!(
            std::ptr::eq(first.as_ptr(), second.as_ptr()),
            "the package was deflated twice for the same profile version",
        );
    }

    #[tokio::test]
    async fn a_changed_profile_is_a_different_package() {
        let before = profile_package("cache-test-changed", files(0))
            .await
            .unwrap();
        let after = profile_package("cache-test-changed", files(60))
            .await
            .unwrap();

        assert!(
            !std::ptr::eq(before.as_ptr(), after.as_ptr()),
            "a file whose timestamp moved should not be served from the cache",
        );
    }

    #[tokio::test]
    async fn the_cache_does_not_grow_without_bound() {
        for index in 0..(MAX_CACHED + 8) {
            profile_package(&format!("cache-test-bound-{index}"), files(0))
                .await
                .unwrap();
        }

        assert!(lock().len() <= MAX_CACHED);
    }
}
