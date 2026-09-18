//! The imports every rustak module starts with.
//!
//! `use rustak_core::prelude::*;` brings in the four things that appear in
//! almost every file — error handling, tracing, serde, and the identity types —
//! so that the imports at the top of a module are the ones specific to *that*
//! module. It deliberately does **not** re-export the config loader, the
//! telemetry session or anything else a handful of files need: a prelude that
//! contains everything is a prelude that tells a reader nothing.
//!
//! ```
//! use rustak_core::prelude::*;
//!
//! #[derive(Deserialize)]
//! struct Enrolment {
//!     username: Username,
//! }
//!
//! let parsed: Enrolment = serde_json::from_str(r#"{"username":"j.smith"}"#).unwrap();
//! assert_eq!(parsed.username.as_str(), "j.smith");
//! ```

/// `human_errors` itself, for `human_errors::user(…)` and `human_errors::system(…)`.
pub use human_errors;

/// `wrap_user_err`, `or_system_err` and the rest of the error-wrapping methods.
pub use human_errors::{Error, OptionExt, ResultExt};

/// `info!`, `warn!`, `instrument`, `Span` and the OpenTelemetry propagation
/// helpers, from `tracing-batteries`' own prelude.
pub use tracing_batteries::prelude::*;

/// The serde traits, including the `DeserializeOwned` bound the config loader
/// and every job payload need.
pub use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub use crate::identity::*;
pub use crate::runtime::Shutdown;
