//! SQLite: the connection pair, the migration runner, the generic stores and
//! the repositories.
//!
//! # Shape
//!
//! [`Database`] is one writer connection plus a small read-only pool, because
//! SQLite has exactly one writer anyway and `tokio-rusqlite` serialises every
//! closure it is handed onto that connection's thread. Sharing one connection
//! between reads and writes would queue every web read behind whatever batch of
//! writes happened to be in flight; sharing many would buy nothing, since the
//! writes still serialise. See `design/01-foundations-storage-ci.md` §4.1.
//!
//! # Rules this module enforces
//!
//! - **Every table is `STRICT`.** A `STRICT` table refuses a value of the wrong
//!   storage class instead of quietly coercing it, which is the difference
//!   between a bug that fails on the way in and one that surfaces months later
//!   as a comparison that never matches.
//! - **Every timestamp is RFC 3339 with milliseconds, bound from Rust.** SQLite's
//!   `CURRENT_TIMESTAMP` resolves only to the second and `DATETIME` is not a
//!   `STRICT` storage class, so timestamps are `TEXT` written through
//!   [`row::Timestamp`]. A migration test refuses any `CURRENT_TIMESTAMP`
//!   default that gets added later. Note that `rusqlite`'s own `chrono` support
//!   writes `2026-09-18 10:00:00.123456789+00:00` — a different separator and a
//!   different precision — so a bare [`chrono::DateTime`] must never be bound
//!   directly.
//! - **`foreign_keys = ON`**, unlike automate, because this schema has
//!   `REFERENCES` clauses and cascades that carry real meaning (deleting a user
//!   takes their devices and credentials with them).
//!
//! # Layout
//!
//! `connection` and `migrations` set the database up; `row` holds the column
//! helpers every repository shares; `kv`, `queue`, `queue_sqlite`, `cache`,
//! `partition` and `audit` are the generic stores lifted from automate with its
//! `tenant` column removed; `repos` holds one repository per aggregate.

pub mod audit;
pub mod cache;
pub mod connection;
pub mod kv;
pub mod migrations;
pub mod partition;
pub mod queue;
pub mod queue_sqlite;
pub mod repos;
pub mod row;

pub use audit::{AuditEntry, AuditQuery, AuditStore};
pub use cache::Cache;
pub use connection::{Checkpoint, Database};
pub use kv::{KeyValueStore, StateKey};
pub use partition::Partition;
pub use queue::{PeekedMessage, Queue, QueueMessage};
pub use repos::Page;
pub use row::Timestamp;

/// Advice offered when a database operation fails for reasons outside our
/// control, such as an unreadable or damaged database file.
pub const ADVICE_DB_ERROR: &[&str] = &[
    "Make sure that the database file is accessible and not corrupted.",
    "If the problem persists, please report the issue to the development team via GitHub.",
];

/// Advice offered when a database operation fails in a way that indicates a bug
/// rather than anything the operator can act on.
pub const ADVICE_REPORT_DEV: &[&str] =
    &["Please report this issue to the development team via GitHub."];
