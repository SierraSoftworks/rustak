//! The content-addressed blob store.
//!
//! Data packages, attachments and profile files are written here rather than
//! into SQLite: they are large, they are immutable, and they are served by
//! streaming a file rather than by reading a row. The name of a blob *is* the
//! SHA-256 of its contents, which makes storing the same package twice free,
//! makes a download verifiable, and means the metadata rows in SQLite only ever
//! carry a hash.
//!
//! A blob lands at `<root>/<first two hex characters>/<full hash>`. The fan-out
//! directory exists because a single directory holding every blob on a busy
//! server is slow to list and, on some filesystems, slow to look up in.
//!
//! # Writing is atomic
//!
//! [`ContentStore::put`] streams into `<root>/tmp/<uuid>` while hashing, then
//! renames the finished file into place. A rename within a filesystem is
//! atomic, so a reader never observes a partially written blob and a crash
//! leaves at worst a stray temporary file. Nothing is ever written to a blob's
//! final path directly.

use std::path::{Path, PathBuf};

use rustak_core::prelude::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// The subdirectory partly written blobs live in until they are renamed.
const TEMP_DIR: &str = "tmp";

/// How many hexadecimal characters of the hash name the fan-out directory.
const FANOUT: usize = 2;

/// The length of a SHA-256 hash rendered as lowercase hexadecimal.
const HASH_CHARS: usize = 64;

/// How much of a stream is read at a time while hashing it.
const COPY_BUFFER: usize = 64 * 1024;

/// Advice for a failure the operator can act on.
const ADVICE_STORAGE: &[&str] = &[
    "Check that the directory in [storage] content_dir exists and is writable by the user rustak runs as.",
    "Check that the filesystem holding it has free space and free inodes.",
];

/// A stored blob: what it is called and how large it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRef {
    /// The SHA-256 of the contents, lowercase hexadecimal.
    pub hash: String,
    /// The size in bytes.
    pub size: u64,
}

/// Immutable blobs addressed by the SHA-256 of their contents.
#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    /// Binds a store to a directory, without touching the filesystem.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory the blobs live in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates the store's directories if they are not already there.
    ///
    /// Called once at startup so that the first upload of an installation's
    /// life fails at boot, where an operator sees it, rather than halfway
    /// through a device's data package transfer.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the directory cannot be
    /// created.
    pub async fn prepare(&self) -> Result<(), Error> {
        let temp = self.root.join(TEMP_DIR);

        tokio::fs::create_dir_all(&temp).await.wrap_user_err(
            format!(
                "We could not create the content directory '{}'.",
                temp.display()
            ),
            ADVICE_STORAGE,
        )
    }

    /// Where a blob with this hash lives.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when `hash` is not a SHA-256
    /// hash. Refusing anything else is what stops a hash taken from a request
    /// path from naming a file outside the store.
    pub fn path_for(&self, hash: &str) -> Result<PathBuf, Error> {
        if hash.len() != HASH_CHARS
            || !hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(human_errors::system(
                "A stored file was addressed by something that is not a SHA-256 hash.",
                &[
                    "This is a bug in the caller; please report it with the surrounding log entries.",
                ],
            ));
        }

        Ok(self.root.join(&hash[..FANOUT]).join(hash))
    }

    /// Streams `reader` into the store, returning the hash it was filed under.
    ///
    /// Storing the same bytes twice is a no-op: the second call hashes the
    /// stream, finds the blob already present, and discards its temporary file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the store cannot be written
    /// to, and a [`human_errors::Kind::System`] error when the source stream
    /// fails partway through.
    #[instrument("store.content.put", skip_all, err(Display))]
    pub async fn put<R: AsyncRead + Unpin>(&self, mut reader: R) -> Result<ContentRef, Error> {
        self.prepare().await?;

        let temp = self
            .root
            .join(TEMP_DIR)
            .join(uuid::Uuid::new_v4().to_string());
        let outcome = self.write_temp(&mut reader, &temp).await;

        let content = match outcome {
            Ok(content) => content,
            Err(err) => {
                // Best effort: the temporary file is already unreachable, and a
                // failure to remove it must not mask the failure that got us
                // here. The orphan sweep collects whatever is left behind.
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(err);
            }
        };

        let destination = self.path_for(&content.hash)?;

        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await.wrap_user_err(
                format!(
                    "We could not create the content directory '{}'.",
                    parent.display()
                ),
                ADVICE_STORAGE,
            )?;
        }

        if tokio::fs::try_exists(&destination).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&temp).await;
            return Ok(content);
        }

        // Atomic within a filesystem, which is why the temporary directory is
        // inside the store rather than in the system temporary directory.
        tokio::fs::rename(&temp, &destination).await.wrap_user_err(
            format!("We could not store a file as '{}'.", destination.display()),
            ADVICE_STORAGE,
        )?;

        Ok(content)
    }

    /// Stores bytes already held in memory.
    ///
    /// # Errors
    ///
    /// As [`ContentStore::put`].
    pub async fn put_bytes(&self, bytes: &[u8]) -> Result<ContentRef, Error> {
        self.put(bytes).await
    }

    /// Opens a stored blob for reading.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the blob is missing — which is
    /// what a metadata row pointing at a swept or deleted file looks like.
    pub async fn open(&self, hash: &str) -> Result<tokio::fs::File, Error> {
        let path = self.path_for(hash)?;

        tokio::fs::File::open(&path).await.wrap_user_err(
            "The file you asked for is no longer stored on this server.",
            &[
                "The file may have been removed by the retention policy.",
                "Ask whoever shared it to upload it again.",
            ],
        )
    }

    /// Whether a blob is stored.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when `hash` is malformed.
    pub async fn exists(&self, hash: &str) -> Result<bool, Error> {
        let path = self.path_for(hash)?;

        Ok(tokio::fs::try_exists(&path).await.unwrap_or(false))
    }

    /// Removes a blob, reporting whether one was there.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the blob exists but cannot be
    /// removed.
    #[instrument("store.content.remove", skip_all, err(Display))]
    pub async fn remove(&self, hash: &str) -> Result<bool, Error> {
        let path = self.path_for(hash)?;

        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).wrap_user_err(
                format!("We could not remove the stored file '{}'.", path.display()),
                ADVICE_STORAGE,
            ),
        }
    }

    /// Every blob the store holds, for the orphan sweep to compare against the
    /// hashes the database still references.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the store cannot be listed.
    #[instrument("store.content.iter", skip_all, err(Display))]
    pub async fn iter(&self) -> Result<Vec<ContentRef>, Error> {
        let mut found = Vec::new();

        let Some(mut fanout) = self.read_dir(&self.root).await? else {
            return Ok(found);
        };

        while let Some(entry) = self.next_entry(&mut fanout, &self.root).await? {
            let directory = entry.path();

            if entry.file_name() == TEMP_DIR {
                continue;
            }

            let Some(mut blobs) = self.read_dir(&directory).await? else {
                continue;
            };

            while let Some(blob) = self.next_entry(&mut blobs, &directory).await? {
                let Some(name) = blob.file_name().to_str().map(str::to_owned) else {
                    continue;
                };

                // Anything that is not named like a hash was not put here by
                // us, so leaving it alone is safer than reporting it to a
                // sweep that would delete it.
                if self.path_for(&name).is_err() {
                    continue;
                }

                let size = match blob.metadata().await {
                    Ok(metadata) if metadata.is_file() => metadata.len(),
                    _ => continue,
                };

                found.push(ContentRef { hash: name, size });
            }
        }

        Ok(found)
    }

    /// Copies a stream into `temp` while hashing it.
    async fn write_temp<R: AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        temp: &Path,
    ) -> Result<ContentRef, Error> {
        let mut file = tokio::fs::File::create(temp).await.wrap_user_err(
            format!("We could not open '{}' to store a file.", temp.display()),
            ADVICE_STORAGE,
        )?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; COPY_BUFFER];
        let mut size = 0u64;

        loop {
            let read = reader.read(&mut buffer).await.or_system_err(&[
                "The upload was interrupted before it finished; ask the sender to try again.",
            ])?;

            if read == 0 {
                break;
            }

            hasher.update(&buffer[..read]);
            size += read as u64;

            file.write_all(&buffer[..read]).await.wrap_user_err(
                format!("We could not write to '{}'.", temp.display()),
                ADVICE_STORAGE,
            )?;
        }

        // Flushed rather than fsynced: a blob whose metadata row never lands is
        // an orphan the sweep collects, so the cost of an fsync per upload buys
        // nothing the recovery path does not already give us.
        file.flush().await.wrap_user_err(
            format!("We could not finish writing '{}'.", temp.display()),
            ADVICE_STORAGE,
        )?;

        Ok(ContentRef {
            hash: hex::encode(hasher.finalize()),
            size,
        })
    }

    /// Opens a directory, treating "not there" as "empty".
    async fn read_dir(&self, path: &Path) -> Result<Option<tokio::fs::ReadDir>, Error> {
        match tokio::fs::read_dir(path).await {
            Ok(entries) => Ok(Some(entries)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).wrap_user_err(
                format!(
                    "We could not list the content directory '{}'.",
                    path.display()
                ),
                ADVICE_STORAGE,
            ),
        }
    }

    /// Reads the next directory entry.
    async fn next_entry(
        &self,
        entries: &mut tokio::fs::ReadDir,
        path: &Path,
    ) -> Result<Option<tokio::fs::DirEntry>, Error> {
        entries.next_entry().await.wrap_user_err(
            format!(
                "We could not list the content directory '{}'.",
                path.display()
            ),
            ADVICE_STORAGE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SHA-256 of `b"hello"`, from an independent implementation.
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn store() -> (tempfile::TempDir, ContentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::new(dir.path());

        (dir, store)
    }

    #[tokio::test]
    async fn a_blob_is_named_by_its_sha256() {
        let (_dir, store) = store();

        let stored = store.put_bytes(b"hello").await.unwrap();

        assert_eq!(stored.hash, HELLO);
        assert_eq!(stored.size, 5);
        assert!(store.exists(&stored.hash).await.unwrap());
    }

    #[tokio::test]
    async fn a_blob_lands_in_a_fanout_directory() {
        let (dir, store) = store();

        let stored = store.put_bytes(b"hello").await.unwrap();

        assert_eq!(
            store.path_for(&stored.hash).unwrap(),
            dir.path().join("2c").join(HELLO)
        );
    }

    #[tokio::test]
    async fn storing_the_same_bytes_twice_is_a_no_op() {
        let (_dir, store) = store();

        let first = store.put_bytes(b"hello").await.unwrap();
        let second = store.put_bytes(b"hello").await.unwrap();

        assert_eq!(first, second);
        assert_eq!(store.iter().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_blob_reads_back_byte_for_byte() {
        let (_dir, store) = store();
        let payload: Vec<u8> = (0..200_000u32).map(|n| n as u8).collect();

        let stored = store.put(payload.as_slice()).await.unwrap();

        let mut file = store.open(&stored.hash).await.unwrap();
        let mut read = Vec::new();
        file.read_to_end(&mut read).await.unwrap();

        assert_eq!(read, payload, "a blob larger than the copy buffer");
        assert_eq!(stored.size, payload.len() as u64);
    }

    #[tokio::test]
    async fn an_empty_blob_is_storable() {
        let (_dir, store) = store();

        let stored = store.put_bytes(b"").await.unwrap();

        assert_eq!(stored.size, 0);
        assert!(store.exists(&stored.hash).await.unwrap());
    }

    #[tokio::test]
    async fn nothing_is_left_in_the_temporary_directory() {
        let (dir, store) = store();
        store.put_bytes(b"hello").await.unwrap();
        store.put_bytes(b"hello").await.unwrap();

        let mut entries = tokio::fs::read_dir(dir.path().join(TEMP_DIR))
            .await
            .unwrap();

        assert!(entries.next_entry().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn removing_reports_whether_anything_was_there() {
        let (_dir, store) = store();
        let stored = store.put_bytes(b"hello").await.unwrap();

        assert!(store.remove(&stored.hash).await.unwrap());
        assert!(!store.remove(&stored.hash).await.unwrap());
        assert!(!store.exists(&stored.hash).await.unwrap());
    }

    #[tokio::test]
    async fn opening_a_missing_blob_is_a_user_error() {
        let (_dir, store) = store();

        let Err(err) = store.open(HELLO).await else {
            panic!("a blob that was never stored cannot be opened");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
    }

    #[tokio::test]
    async fn a_hash_that_is_not_a_hash_is_refused() {
        let (_dir, store) = store();

        // The property that matters: a value taken from a request path cannot
        // escape the store, whatever separators or casing it carries.
        for attempt in [
            "../../etc/passwd",
            "2c/../../etc/passwd",
            "",
            "2cf24dba",
            &HELLO.to_uppercase(),
            &format!("{HELLO}0"),
        ] {
            assert!(
                store.path_for(attempt).is_err(),
                "{attempt} should be refused"
            );
            assert!(
                store.exists(attempt).await.is_err(),
                "{attempt} should be refused"
            );
        }
    }

    #[tokio::test]
    async fn listing_an_absent_store_is_empty_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::new(dir.path().join("never-created"));

        assert!(store.iter().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn listing_reports_every_blob_and_ignores_anything_else() {
        let (dir, store) = store();
        let first = store.put_bytes(b"hello").await.unwrap();
        let second = store.put_bytes(b"goodbye").await.unwrap();

        // A stray file somebody dropped in by hand: reporting it to the orphan
        // sweep would have the sweep delete it.
        tokio::fs::write(dir.path().join("2c").join("notes.txt"), b"hi")
            .await
            .unwrap();

        let mut found = store.iter().await.unwrap();
        found.sort_by(|a, b| a.hash.cmp(&b.hash));

        let mut expected = vec![first, second];
        expected.sort_by(|a, b| a.hash.cmp(&b.hash));

        assert_eq!(found, expected);
    }
}
