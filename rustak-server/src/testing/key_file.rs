//! Where [`keys`](super::keys) keeps a key between test processes, and how
//! many processes starting at once agree on who makes it.
//!
//! A key file is written to a name no other writer uses and renamed into
//! place, so a reader sees no file or a whole one — never half of one. A file
//! that does not parse is treated as missing and replaced.
//!
//! On a cold build every process that wants a key finds none. Left alone, each
//! would generate its own and race to the rename: harmless, since no two
//! processes need to agree on a key, but under coverage four processes each
//! spending seconds on the same four keys is most of a minute of the run. So
//! the first to find no file takes `<name>.lock` (an exclusive create) and
//! makes the key; the others wait for the file to appear and read it. A lock
//! older than [`STALE`] was left by a process that was killed mid-generation,
//! and is taken over. A waiter that runs out of patience, or a directory that
//! cannot be written, only costs a generation of its own.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How old a lock must be before it is taken to belong to a dead process, and
/// how long a waiter waits for a live one before making the key itself.
///
/// Neither outcome fails anything, so this bounds wasted time, not a test: a
/// generation takes a second or two under coverage, and a host ten times
/// slower still finishes well inside it.
const STALE: Duration = Duration::from_secs(60);

/// How often a waiter looks for the key file.
const POLL: Duration = Duration::from_millis(25);

/// The directory under `<target>/<profile>/` the keys are kept in.
pub(super) const DIRECTORY: &str = "rustak-test-keys";

/// Where this process keeps its keys, if it is a test binary Cargo built.
///
/// Test binaries live in `<target>/<profile>/deps/`. `CARGO_TARGET_TMPDIR`
/// would be the obvious place, but Cargo sets it only when compiling
/// integration tests and benchmarks, not the library these keys are made in.
pub(super) fn directory() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let deps = executable.parent()?;

    if deps.file_name()? != "deps" {
        return None;
    }

    Some(deps.parent()?.join(DIRECTORY))
}

/// Reads `name` from `directory`, or makes it and leaves it there for the
/// next process.
///
/// Generic over the key so that the file handling can be tested with
/// something cheaper to make than an RSA key.
pub(super) fn load_or_make<T>(
    directory: &Path,
    name: &str,
    parse: impl Fn(&[u8]) -> Option<T>,
    make: impl FnOnce() -> Vec<u8>,
) -> T {
    let path = directory.join(format!("{name}.pk8"));
    let lock = directory.join(format!("{name}.lock"));
    let read = || std::fs::read(&path).ok().and_then(|bytes| parse(&bytes));
    let waiting_since = Instant::now();

    let held = loop {
        if let Some(key) = read() {
            return key;
        }

        match create_private_dir(directory).and_then(|()| open_private(&lock)) {
            Ok(_) => break Some(Held(&lock)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale(&lock) {
                    let _ = std::fs::remove_file(&lock);
                } else if waiting_since.elapsed() < STALE {
                    std::thread::sleep(POLL);
                } else {
                    break None;
                }
            }
            // A directory we cannot write to: make the key and keep it.
            Err(_) => break None,
        }
    };

    // Somebody may have finished between our last look and taking the lock.
    if held.is_some()
        && let Some(key) = read()
    {
        return key;
    }

    let bytes = make();
    let key = parse(&bytes).expect("a key that was just made parses");

    // Best effort: a file we cannot write costs the next process a
    // generation, which is what every process paid before there was a file.
    let _ = store(directory, &path, &bytes);
    drop(held);

    key
}

/// A lock this process holds, released when it is dropped.
struct Held<'a>(&'a Path);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Whether a lock is old enough that its holder must be gone.
fn is_stale(lock: &Path) -> bool {
    std::fs::metadata(lock)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > STALE)
}

/// Distinguishes the temporary files of writers in one process.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` so that no reader ever sees part of them.
fn store(directory: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // A name no other writer uses — this process, this write — in the same
    // directory, so the rename below is within one filesystem and atomic.
    let temporary = directory.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("key"),
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed),
    ));

    let written = create_private_dir(directory)
        .and_then(|()| open_private(&temporary))
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&temporary, path));

    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }

    written
}

/// Creates `directory`, owner-only where the platform has owners.
fn create_private_dir(directory: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);

    builder.create(directory)
}

/// Creates a file that must not exist yet, owner-only where the platform has
/// owners.
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// A stand-in key: thirty-two bytes, cheap to make and easy to tell apart.
    fn fake(bytes: &[u8]) -> Option<Vec<u8>> {
        (bytes.len() == 32).then(|| bytes.to_vec())
    }

    /// A different stand-in key on every call, counted in `made`.
    fn maker(made: &AtomicUsize) -> impl Fn() -> Vec<u8> + '_ {
        || {
            let serial = made.fetch_add(1, Ordering::Relaxed) as u64;
            let mut bytes = [0xa5u8; 32];
            bytes[..8].copy_from_slice(&serial.to_le_bytes());

            bytes.to_vec()
        }
    }

    /// The files in `directory`, sorted.
    fn files(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();

        names
    }

    #[test]
    fn a_key_is_made_once_and_read_back_after() {
        let directory = tempfile::tempdir().unwrap();
        let made = AtomicUsize::new(0);

        let first = load_or_make(directory.path(), "k", fake, maker(&made));
        let second = load_or_make(directory.path(), "k", fake, maker(&made));

        assert_eq!(first, second);
        assert_eq!(
            made.load(Ordering::Relaxed),
            1,
            "the second call read the file"
        );
        assert_eq!(
            files(directory.path()),
            ["k.pk8"],
            "no lock or temporary is left"
        );
    }

    #[test]
    fn two_names_are_two_keys() {
        let directory = tempfile::tempdir().unwrap();
        let made = AtomicUsize::new(0);

        let one = load_or_make(directory.path(), "one", fake, maker(&made));
        let other = load_or_make(directory.path(), "other", fake, maker(&made));

        assert_ne!(one, other);
    }

    #[test]
    fn a_file_that_does_not_parse_is_made_again_and_replaced() {
        // A key from some other build, or anything else that is not a key:
        // the next process must not trip over it for ever.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("k.pk8");
        std::fs::write(&path, b"not a key").unwrap();

        let key = load_or_make(directory.path(), "k", fake, maker(&AtomicUsize::new(0)));

        assert_eq!(
            std::fs::read(&path).unwrap(),
            key,
            "the file now holds the key"
        );
    }

    #[test]
    fn many_processes_starting_at_once_make_one_key_and_all_use_it() {
        // What a cold nextest run does: every process finds no file at the
        // same moment. One makes the key; the rest wait for it and read it.
        // Nothing here is timed — however the threads interleave, a second
        // maker would show up in the count.
        let directory = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(16);
        let made = AtomicUsize::new(0);

        let keys: Vec<Vec<u8>> = std::thread::scope(|scope| {
            let writers: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        load_or_make(directory.path(), "k", fake, maker(&made))
                    })
                })
                .collect();

            writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .collect()
        });

        let left = std::fs::read(directory.path().join("k.pk8")).unwrap();

        assert_eq!(made.load(Ordering::Relaxed), 1);
        assert!(keys.iter().all(|key| *key == left));
        assert_eq!(files(directory.path()), ["k.pk8"]);
    }

    #[test]
    fn a_lock_left_by_a_process_that_died_is_taken_over() {
        // Killed mid-generation, it never removed its lock. Without the age
        // check every later process would wait out the whole of STALE.
        let directory = tempfile::tempdir().unwrap();
        let lock = directory.path().join("k.lock");
        let file = std::fs::File::create(&lock).unwrap();
        file.set_modified(SystemTime::now() - STALE * 2).unwrap();
        drop(file);

        let made = AtomicUsize::new(0);
        load_or_make(directory.path(), "k", fake, maker(&made));

        assert_eq!(made.load(Ordering::Relaxed), 1);
        assert_eq!(files(directory.path()), ["k.pk8"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let keys = directory.path().join(DIRECTORY);

        load_or_make(&keys, "k", fake, maker(&AtomicUsize::new(0)));

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode(&keys), 0o700);
        assert_eq!(mode(&keys.join("k.pk8")), 0o600);
    }
}
