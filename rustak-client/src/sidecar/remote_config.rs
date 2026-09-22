//! The configuration an administrator edits in the admin UI, described by the
//! struct the plugin already reads it into.
//!
//! `GET /api/v1/services/<name>/config` is an opaque JSON object as far as the
//! wire is concerned, which is fine for a transport and hopeless for a person:
//! nobody knows which keys exist, and a typo is a setting that silently does
//! nothing. So a plugin says what it reads, in the one place that cannot drift
//! from what it reads — the type:
//!
//! ```
//! use rustak_client::sidecar::{JsonSchema, parse_config, schema_for};
//! use serde::Deserialize;
//!
//! /// What an administrator may change while this plugin is running.
//! #[derive(Deserialize, JsonSchema)]
//! #[serde(deny_unknown_fields)]
//! struct Tuning {
//!     /// How often to poll the upstream, in seconds.
//!     #[schemars(range(min = 5, max = 3600))]
//!     interval_seconds: u64,
//! }
//!
//! let schema = schema_for::<Tuning>();
//! assert_eq!(schema["properties"]["interval_seconds"]["minimum"], 5);
//!
//! assert!(parse_config::<Tuning>(&serde_json::json!({ "interval_seconds": 30 })).is_ok());
//! assert!(parse_config::<Tuning>(&serde_json::json!({ "interval": 30 })).is_err());
//! ```
//!
//! [`Sidecar::config_schema`](super::Sidecar::config_schema) hands the schema to
//! the harness, which registers it; the admin UI draws a form from it — doc
//! comments become the help text — and the server holds every write to it.
//! [`Sidecar::validate_config`](super::Sidecar::validate_config) is for what a
//! schema cannot say.
//!
//! A sidecar not written against this SDK takes part by putting any JSON Schema
//! in its descriptor's `config_schema`; nothing here is on the wire.

pub use schemars::{self, JsonSchema};

use rustak_api::{ConfigIssue, ConfigValidation};
use rustak_core::prelude::*;

/// The JSON Schema (2020-12) for `T`, as [`Sidecar::config_schema`] wants it.
///
/// [`Sidecar::config_schema`]: super::Sidecar::config_schema
#[must_use]
pub fn schema_for<T: JsonSchema>() -> serde_json::Value {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

/// Reads a configuration document into `T`, or says why it could not be.
///
/// The first line of most [`Sidecar::validate_config`] hooks: the server has
/// already held the candidate to the schema, but only deserialising it proves
/// that this build can read it — and hands back the value to check further.
///
/// # Errors
///
/// A rejection carrying serde's own words, ready to be answered with.
///
/// [`Sidecar::validate_config`]: super::Sidecar::validate_config
pub fn parse_config<T: DeserializeOwned>(
    config: &serde_json::Value,
) -> Result<T, ConfigValidation> {
    T::deserialize(config).map_err(|err| {
        ConfigIssue::new(format!("This is not a configuration we can read: {err}.")).into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a plugin's own type: a tagged enum and an optional field
    /// are the two shapes the admin UI's form has to understand.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Tuning {
        /// How often to poll, in seconds.
        #[schemars(range(min = 5))]
        interval_seconds: u32,

        #[serde(default)]
        source: Option<Source>,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Source {
        Replay { path: String },
        Live,
    }

    #[test]
    fn the_schema_says_what_the_struct_says() {
        let schema = schema_for::<Tuning>();
        let interval = &schema["properties"]["interval_seconds"];

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["interval_seconds"]));
        assert_eq!(interval["minimum"], 5);
        assert_eq!(interval["description"], "How often to poll, in seconds.");
    }

    #[test]
    fn a_document_the_type_cannot_read_is_a_rejection_rather_than_an_error() {
        let read = parse_config::<Tuning>(&serde_json::json!({
            "interval_seconds": 30,
            "source": { "kind": "replay", "path": "tracks.json" },
        }));
        assert!(matches!(
            read,
            Ok(Tuning { interval_seconds: 30, source: Some(Source::Replay { path }) })
                if path == "tracks.json"
        ));

        let refused = parse_config::<Tuning>(&serde_json::json!({ "interval_secs": 30 }))
            .expect_err("a misspelled key");
        assert!(!refused.is_valid());
        assert!(
            refused.issues[0].message.contains("interval_secs"),
            "{refused:?}"
        );
    }
}
