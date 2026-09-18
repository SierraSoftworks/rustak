//! The two stores that are deliberately not SQLite.
//!
//! The database holds state and metadata: who is enrolled, which mission a
//! package belongs to, where a segment file is. It does not hold the bytes.
//! Two kinds of data are kept out of it entirely, for two different reasons.
//!
//! [`ContentStore`] holds **immutable blobs** — data packages, attachments,
//! profile files — under the SHA-256 of their contents. They are large, they
//! are served by streaming a file rather than reading a row, and addressing
//! them by hash makes storing a duplicate free and a download verifiable.
//!
//! [`AppendLog`] holds **time series** — CoT history now, telemetry later — as
//! rolling segment files of length-prefixed records, indexed in SQLite by
//! `stream_segments`. A busy installation writes thousands of positions a
//! second; routing those through the single SQLite writer would put every web
//! read behind them. Here, appending is a write to the end of a file and
//! retention is an `unlink`.
//!
//! Both are plain directory trees. An operator can list them, back them up
//! with `rsync` and reason about their size without a tool that understands
//! rustak, which is worth more than any format we could have invented.

pub mod append_log;
pub mod content;
mod frame;
mod segment;

pub use append_log::{AppendLog, AppendLogOptions, DEFAULT_SEGMENT_BYTES};
pub use content::{ContentRef, ContentStore};
