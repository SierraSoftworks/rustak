//! Getting bytes into and out of the content store without holding them.
//!
//! A data package is routinely tens of megabytes and the configured ceiling is
//! four hundred. Buffering one in memory would mean a server whose resident set
//! is the number of EUDs sharing packages times the size of the largest one, so
//! nothing here ever collects a body: an upload is a stream that is hashed and
//! written to a temporary file as it arrives ([`ingest`]), and a download is a
//! file read a chunk at a time ([`open_range`], [`chunks`]).
//!
//! # Why the limit lives inside the reader
//!
//! `Content-Length` is a claim. A chunked upload does not carry one at all, and
//! one that does can lie — so checking the header and then reading whatever
//! arrives would leave the ceiling unenforced for exactly the request that
//! meant to exceed it. [`BoundedReader`] counts what it actually passes on and
//! fails the read the moment it goes over, which means the temporary file stops
//! growing there rather than at the end of a 4 GB body.
//!
//! The header is *also* checked, in the handler, because refusing before the
//! upload starts is much kinder than refusing after it.

use std::pin::Pin;
use std::task::{Context, Poll};

use actix_web::web::Bytes;
use futures::Stream;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeekExt as _, ReadBuf};

use crate::prelude::*;
use crate::services::ContentStore;

/// How much of a stored file is read at a time when serving a download.
const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// A stored upload: what it was filed under and how big it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// The SHA-256 of the contents, lowercase hexadecimal.
    pub hash: String,
    /// The size in bytes.
    pub size: u64,
}

/// Why an upload was not stored.
#[derive(Debug)]
pub enum IngestError {
    /// More bytes arrived than the configured ceiling allows.
    TooLarge,
    /// Nothing arrived at all, which TAK refuses rather than storing an empty
    /// resource a client would then fail to download.
    Empty,
    /// The stream failed partway through, or the store could not be written to.
    Failed(Error),
}

impl From<Error> for IngestError {
    fn from(err: Error) -> Self {
        Self::Failed(err)
    }
}

/// Streams a body into the store, hashing it on the way.
///
/// The bytes land in the store's own temporary directory and are renamed into
/// place under their hash, so a blob is never observed half-written and storing
/// the same package twice costs one hash rather than one copy.
///
/// # Errors
///
/// [`IngestError::TooLarge`] past `limit` bytes, [`IngestError::Empty`] for a
/// body with no content, and [`IngestError::Failed`] for a broken stream or an
/// unwritable store.
#[instrument("files.store.ingest", skip_all, fields(limit = limit))]
pub async fn ingest<S>(store: &ContentStore, body: S, limit: u64) -> Result<Ingested, IngestError>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    let mut reader = BoundedReader::new(body, limit);
    let stored = store.put(&mut reader).await;

    // Asked before the error is unwrapped: the store reports the read failure
    // the bound raised as an ordinary broken stream, and only the reader knows
    // which of the two it was.
    if reader.exceeded {
        return Err(IngestError::TooLarge);
    }

    let stored = stored?;

    if stored.size == 0 {
        return Err(IngestError::Empty);
    }

    Ok(Ingested {
        hash: stored.hash,
        size: stored.size,
    })
}

/// A stored file positioned at the start of the range a request asked for.
#[derive(Debug)]
pub struct Opened {
    /// The file, already sought to [`Opened::offset`].
    pub file: tokio::fs::File,
    /// Where the served range begins.
    pub offset: u64,
    /// How many bytes are to be served.
    pub length: u64,
    /// How large the whole file is.
    pub total: u64,
}

impl Opened {
    /// Whether this is less than the whole file, and so a `206`.
    pub fn is_partial(&self) -> bool {
        self.offset > 0 || self.length < self.total
    }

    /// The `Content-Range` value for a partial answer.
    pub fn content_range(&self) -> String {
        let last = (self.offset + self.length)
            .saturating_sub(1)
            .max(self.offset);

        format!("bytes {}-{last}/{}", self.offset, self.total)
    }
}

/// Opens a stored blob, honouring an `offset` and a `length`.
///
/// An offset past the end of the file serves nothing rather than failing: a
/// client resuming a download of a file that has since been replaced should get
/// an empty body, not a `500`.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the blob is not stored, which is
/// what a metadata row pointing at a swept file looks like.
pub async fn open_range(
    store: &ContentStore,
    hash: &str,
    offset: u64,
    length: Option<u64>,
) -> Result<Opened, Error> {
    let mut file = store.open(hash).await?;
    let total = file
        .metadata()
        .await
        .or_system_err(&["The stored file could not be measured."])?
        .len();

    let offset = offset.min(total);

    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .or_system_err(&["The stored file could not be read from that position."])?;
    }

    let remaining = total - offset;

    Ok(Opened {
        file,
        offset,
        length: length.map_or(remaining, |asked| asked.min(remaining)),
        total,
    })
}

/// Reads a file out as a stream of chunks, stopping after `length` bytes.
///
/// `actix_web::HttpResponse::streaming` takes this directly, so the body of a
/// download is never assembled anywhere.
pub fn chunks(
    file: tokio::fs::File,
    length: u64,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    futures::stream::unfold((file, length), |(mut file, remaining)| async move {
        if remaining == 0 {
            return None;
        }

        let wanted = DOWNLOAD_CHUNK.min(usize::try_from(remaining).unwrap_or(DOWNLOAD_CHUNK));
        let mut buffer = vec![0u8; wanted];

        match file.read(&mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);

                Some((Ok(Bytes::from(buffer)), (file, remaining - read as u64)))
            }
            // The stream ends after the failure; a partial body with a logged
            // error beats a panic in a worker.
            Err(err) => Some((Err(err), (file, 0))),
        }
    })
}

/// Removes a blob once no row, mission attachment or profile file wants it.
///
/// Called after a resource row goes. A hash backs as many rows as somebody has
/// uploaded the same file under, so the bytes outlive any one of them.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the database cannot be asked, and
/// a [`human_errors::Kind::User`] error if the blob is there and will not go.
#[instrument("files.store.forget", skip_all, fields(file.hash = hash))]
pub async fn forget(services: &impl Services, hash: &str) -> Result<bool, Error> {
    if services.db().resources().hash_in_use(hash).await? {
        return Ok(false);
    }

    services.content()?.remove(hash).await
}

/// An [`AsyncRead`] over a body stream that refuses to pass on too much.
///
/// Public so that a test can drive it directly; handlers reach it through
/// [`ingest`].
pub struct BoundedReader<S> {
    stream: S,
    pending: Bytes,
    read: u64,
    limit: u64,
    exceeded: bool,
}

impl<S> BoundedReader<S> {
    /// Wraps a stream with a ceiling in bytes.
    pub fn new(stream: S, limit: u64) -> Self {
        Self {
            stream,
            pending: Bytes::new(),
            read: 0,
            limit,
            exceeded: false,
        }
    }

    /// Whether the ceiling was reached, which is why the read failed.
    pub fn exceeded(&self) -> bool {
        self.exceeded
    }
}

impl<S> AsyncRead for BoundedReader<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        loop {
            if !this.pending.is_empty() {
                let take = this.pending.len().min(buf.remaining());
                buf.put_slice(&this.pending.split_to(take));

                return Poll::Ready(Ok(()));
            }

            match Pin::new(&mut this.stream).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    // An empty chunk is not the end of the stream, so it must
                    // not be reported as one.
                    if chunk.is_empty() {
                        continue;
                    }

                    this.read += chunk.len() as u64;

                    if this.read > this.limit {
                        this.exceeded = true;

                        return Poll::Ready(Err(std::io::Error::other(
                            "the upload is larger than this server accepts",
                        )));
                    }

                    this.pending = chunk;
                }
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Err(err)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;

    use super::*;

    /// The SHA-256 of `b"hello"`, from an independent implementation.
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn store() -> (tempfile::TempDir, ContentStore) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = ContentStore::new(dir.path());

        (dir, store)
    }

    fn body(
        chunks: Vec<&'static [u8]>,
    ) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        futures::stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok(Bytes::from_static(chunk))),
        )
    }

    #[tokio::test]
    async fn a_body_that_arrives_in_pieces_is_hashed_whole() {
        let (_dir, store) = store();

        let stored = ingest(&store, body(vec![b"he", b"", b"llo"]), 1024)
            .await
            .expect("a body under the limit is stored");

        assert_eq!(stored.hash, HELLO);
        assert_eq!(stored.size, 5);
    }

    #[tokio::test]
    async fn the_ceiling_is_enforced_on_what_arrives_rather_than_what_was_claimed() {
        let (_dir, store) = store();

        let outcome = ingest(&store, body(vec![b"hello", b"world"]), 6).await;

        assert!(
            matches!(outcome, Err(IngestError::TooLarge)),
            "a body past the ceiling is refused, not truncated",
        );
    }

    #[tokio::test]
    async fn a_body_with_no_content_is_refused() {
        let (_dir, store) = store();

        let outcome = ingest(&store, body(vec![]), 1024).await;

        assert!(matches!(outcome, Err(IngestError::Empty)));
    }

    #[tokio::test]
    async fn a_multi_megabyte_body_never_lands_in_one_buffer() {
        // The property the interop suite's large upload also asserts: what the
        // ingest holds is one chunk, whatever the body's total size.
        let (_dir, store) = store();
        let chunk = vec![7u8; 64 * 1024];
        let count = 64; // 4 MiB
        let stream = futures::stream::iter(
            std::iter::repeat_n(chunk.clone(), count).map(|c| Ok(Bytes::from(c))),
        );

        let stored = ingest(&store, stream, 16 * 1024 * 1024).await.unwrap();

        assert_eq!(stored.size, (chunk.len() * count) as u64);
        assert!(store.exists(&stored.hash).await.unwrap());
    }

    #[tokio::test]
    async fn a_range_is_served_from_the_middle_of_the_file() {
        let (_dir, store) = store();
        let stored = store.put_bytes(b"0123456789").await.unwrap();

        let opened = open_range(&store, &stored.hash, 3, Some(4)).await.unwrap();

        assert!(opened.is_partial());
        assert_eq!(opened.content_range(), "bytes 3-6/10");

        let read: Vec<u8> = chunks(opened.file, opened.length)
            .map(|chunk| chunk.unwrap().to_vec())
            .concat()
            .await;

        assert_eq!(read, b"3456");
    }

    #[tokio::test]
    async fn no_range_is_the_whole_file_and_not_partial() {
        let (_dir, store) = store();
        let stored = store.put_bytes(b"0123456789").await.unwrap();

        let opened = open_range(&store, &stored.hash, 0, None).await.unwrap();

        assert!(!opened.is_partial());
        assert_eq!(opened.length, 10);
    }

    #[tokio::test]
    async fn an_offset_past_the_end_serves_nothing_rather_than_failing() {
        let (_dir, store) = store();
        let stored = store.put_bytes(b"0123456789").await.unwrap();

        let opened = open_range(&store, &stored.hash, 99, Some(5)).await.unwrap();

        assert_eq!(opened.length, 0);
        assert_eq!(opened.offset, 10);
    }
}
