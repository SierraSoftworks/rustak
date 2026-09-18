//! The imports a `rustak-server` module starts with.
//!
//! `use crate::prelude::*;` brings in [`rustak_core::prelude`] — errors,
//! tracing, serde and the identity types — together with the handful of
//! server-side types that appear in almost every file: the configuration, the
//! services handle and the storage traits whose methods are reached through it.
//!
//! It stops there on purpose. The repositories, the crypto types and the web
//! helpers are each used by a minority of files, and a prelude that contains
//! everything tells a reader nothing about what a module actually does.
//!
//! ```
//! use rustak_server::prelude::*;
//!
//! #[derive(Deserialize)]
//! struct Enrolment {
//!     username: Username,
//! }
//!
//! let parsed: Enrolment = serde_json::from_str(r#"{"username":"j.smith"}"#).unwrap();
//! assert_eq!(parsed.username.as_str(), "j.smith");
//! ```

/// Errors, tracing, serde, the identity newtypes and [`Shutdown`].
///
/// [`Shutdown`]: rustak_core::runtime::Shutdown
pub use rustak_core::prelude::*;

/// The parsed configuration file, reached through
/// [`Services::config`].
pub use crate::config::Config;

/// The storage traits. They are traits, so their methods are only in scope
/// where the trait is — which is why they are here rather than left to each
/// call site.
pub use crate::db::{AuditStore, Cache, KeyValueStore, Queue};

/// Writing a background job and dispatching one.
pub use crate::jobs::{Job, JobContext};

/// The services handle every module is written against.
pub use crate::services::{AppContext, Services};
