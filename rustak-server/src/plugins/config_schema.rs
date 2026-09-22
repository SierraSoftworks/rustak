//! Holding a service's configuration to the JSON Schema that service registered.
//!
//! The schema is the service's own statement of what it can read, so it is
//! checked twice: once when it arrives ([`check`]), so that a schema nobody
//! could ever satisfy — or that is not a schema — is refused at registration
//! where the plugin's author will see it, and once per candidate ([`issues`]),
//! which is what `PUT …/config` and `POST …/config/validate` both answer from.
//!
//! # A schema is input, and is treated as input
//!
//! It comes from a service account, which is authenticated but is not an
//! administrator. So it is bounded ([`MAX_SCHEMA_BYTES`]), it can never make
//! this server fetch anything (`jsonschema` is built without its HTTP and file
//! resolvers, so a remote `$ref` fails to compile rather than dialling out),
//! and its `pattern`s run on the linear-time `regex` engine rather than the
//! backtracking one, which a hostile pattern could otherwise pin a worker with.
//!
//! # Values are never echoed
//!
//! A configuration may hold an upstream credential, and these messages travel
//! into a `tracing` span on the way to the administrator. They are rendered
//! masked: *where* and *why*, never *what*.

use jsonschema::{PatternOptions, Validator};
use rustak_api::ConfigIssue;

use crate::prelude::*;

/// The largest schema a service may register.
///
/// A generous multiple of what `schemars` derives for a real settings struct,
/// and small enough that compiling one per validation is not worth caching.
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;

/// How many problems are reported for one candidate. A form shows them beside
/// the fields they are about, and nobody fixes more than this at a time.
const MAX_ISSUES: usize = 32;

/// Compiles a schema, with the restrictions the module documentation names.
fn validator(schema: &serde_json::Value) -> Result<Validator, String> {
    jsonschema::options()
        .with_pattern_options(PatternOptions::regex())
        .build(schema)
        .map_err(|err| err.masked().to_string())
}

/// Whether the schema's root could accept a JSON object, which is the only
/// thing `PUT …/config` lets a configuration be.
///
/// Not a proof — a `oneOf` of scalars gets past it — but it refuses the root
/// that names another type, pins a scalar `const`, or lists an `enum` with no
/// object in it: schemas that would register and then refuse every save.
fn admits_an_object(schema: &serde_json::Value) -> bool {
    let named = match schema.get("type") {
        Some(serde_json::Value::String(name)) => name == "object",
        Some(serde_json::Value::Array(names)) => {
            names.iter().any(|name| name.as_str() == Some("object"))
        }
        _ => true,
    };
    let pinned = schema.get("const").is_none_or(serde_json::Value::is_object);
    let listed = schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|values| values.iter().any(serde_json::Value::is_object));

    named && pinned && listed
}

/// Whether `schema` is one a configuration could be held to.
///
/// # Errors
///
/// What is wrong with it, in words for the plugin's author.
pub fn check(schema: &serde_json::Value) -> Result<(), String> {
    if !schema.is_object() {
        return Err("A configuration schema has to be a JSON Schema object.".to_string());
    }

    if !admits_an_object(schema) {
        return Err(
            "A service's configuration is a JSON object, and that schema could never accept one."
                .to_string(),
        );
    }

    let size = schema.to_string().len();
    if size > MAX_SCHEMA_BYTES {
        return Err(format!(
            "That configuration schema is {size} bytes, and the largest we accept is {MAX_SCHEMA_BYTES}."
        ));
    }

    validator(schema)
        .map(drop)
        .map_err(|err| format!("That configuration schema is not one we can use: {err}"))
}

/// What is wrong with `config`, according to `schema`.
///
/// Nothing, for a service that registered no schema: its configuration is
/// free-form, as every service's was before schemas existed.
pub fn issues(schema: Option<&serde_json::Value>, config: &serde_json::Value) -> Vec<ConfigIssue> {
    let Some(schema) = schema else {
        return Vec::new();
    };

    let validator = match validator(schema) {
        Ok(validator) => validator,
        // Checked at registration, so this is a schema that stopped compiling
        // under a newer build of ours. An administrator must still be able to
        // change the configuration; the service re-registers on its next start.
        Err(err) => {
            warn!(error = %err, "A stored configuration schema no longer compiles; not enforcing it.");

            return Vec::new();
        }
    };

    validator
        .iter_errors(config)
        .take(MAX_ISSUES)
        .map(|err| ConfigIssue::at(err.instance_path().as_str(), err.masked().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "api_key": { "type": "string", "minLength": 8 },
                "area": {
                    "type": "object",
                    "properties": { "lat": { "type": "number", "minimum": -90, "maximum": 90 } },
                },
            },
            "required": ["api_key"],
            "additionalProperties": false,
        })
    }

    #[test]
    fn a_configuration_is_held_to_the_schema_and_told_where_it_fell_short() {
        for (config, path) in [
            (serde_json::json!({ "api_key": "long-enough" }), None),
            (serde_json::json!({}), Some("")),
            (
                serde_json::json!({ "api_key": "long-enough", "aera": {} }),
                Some(""),
            ),
            (serde_json::json!({ "api_key": "short" }), Some("/api_key")),
            (
                serde_json::json!({ "api_key": "long-enough", "area": { "lat": 91 } }),
                Some("/area/lat"),
            ),
        ] {
            let found = issues(Some(&schema()), &config);

            match path {
                None => assert!(found.is_empty(), "{config}: {found:?}"),
                Some(path) => assert_eq!(
                    found
                        .first()
                        .map(|issue| issue.path.as_deref().unwrap_or("")),
                    Some(path),
                    "{config}: {found:?}",
                ),
            }
        }
    }

    #[test]
    fn a_service_that_registered_no_schema_keeps_a_free_form_configuration() {
        assert!(issues(None, &serde_json::json!({ "anything": [1, 2, 3] })).is_empty());
    }

    #[test]
    fn a_refused_value_is_never_repeated_back() {
        let found = issues(
            Some(&schema()),
            &serde_json::json!({ "api_key": "hunter2" }),
        );

        assert_eq!(found.len(), 1);
        assert!(!format!("{found:?}").contains("hunter2"), "{found:?}");
    }

    #[test]
    fn a_schema_nobody_could_use_is_refused_when_it_arrives() {
        for (schema, why) in [
            (serde_json::json!([]), "not an object"),
            (
                serde_json::json!({ "type": "string" }),
                "could never accept an object",
            ),
            (
                serde_json::json!({ "enum": ["on", "off"] }),
                "lists no object",
            ),
            (
                serde_json::json!({ "type": "no-such-type" }),
                "not a schema",
            ),
            (
                serde_json::json!({ "$ref": "https://example.com/schema.json" }),
                "would have to be fetched",
            ),
            (
                serde_json::json!({ "description": "x".repeat(MAX_SCHEMA_BYTES) }),
                "too large",
            ),
        ] {
            assert!(check(&schema).is_err(), "{why}");
        }

        assert_eq!(check(&self::schema()), Ok(()));
    }
}
