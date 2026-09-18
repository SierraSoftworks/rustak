//! A small JSON-schema oracle, and the normaliser the goldens are taken with.
//!
//! The schemas in `tests/fixtures/*.schema.json` are transcribed by hand from
//! the TypeBox types in `@tak-ps/node-tak` — the library CloudTAK is written
//! against — so that "CloudTAK would reject this payload" is a test failure
//! here rather than a bug report from somebody's deployment. They are our own
//! files describing a third-party contract, not copies of anybody's code.
//!
//! # Why the validator is written out
//!
//! It handles exactly the subset the fixtures use: `type`, `required`,
//! `properties`, `items`, `enum` and a `$ref` that names another fixture. A
//! general-purpose validator would be a dependency, and the part of JSON Schema
//! that matters here is the part that says which fields a client will look for.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::Value;

/// Every schema, by the name [`validate`] takes.
static SCHEMAS: LazyLock<BTreeMap<&'static str, Value>> = LazyLock::new(|| {
    [
        ("mission", include_str!("../fixtures/mission.schema.json")),
        (
            "mission_change",
            include_str!("../fixtures/mission_change.schema.json"),
        ),
        (
            "mission_subscription",
            include_str!("../fixtures/mission_subscription.schema.json"),
        ),
        ("resource", include_str!("../fixtures/resource.schema.json")),
        ("role", include_str!("../fixtures/role.schema.json")),
    ]
    .into_iter()
    .map(|(name, text)| {
        (
            name,
            serde_json::from_str(text).unwrap_or_else(|err| panic!("{name}.schema.json: {err}")),
        )
    })
    .collect()
});

/// Asserts that a value satisfies one of the transcribed schemas.
///
/// # Panics
///
/// With the path of the first field that is missing or of the wrong type, which
/// is the thing a client would have failed on.
pub fn validate(schema: &str, value: &Value) {
    let schema = SCHEMAS
        .get(schema)
        .unwrap_or_else(|| panic!("no fixture named {schema}.schema.json"));

    check(
        schema,
        value,
        schema["title"].as_str().unwrap_or(schema_name(schema)),
    );
}

/// The schema's own name, for an error path.
fn schema_name(schema: &Value) -> &str {
    schema["title"].as_str().unwrap_or("value")
}

/// Checks one value against one schema node.
fn check(schema: &Value, value: &Value, path: &str) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let referenced = SCHEMAS
            .get(reference)
            .unwrap_or_else(|| panic!("{path}: no fixture named {reference}.schema.json"));

        check(referenced, value, path);

        return;
    }

    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        assert!(
            matches_type(expected, value),
            "{path} should be a {expected}, and is {value}",
        );
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        assert!(allowed.contains(value), "{path} is not one of {allowed:?}");
    }

    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        assert!(
            value.get(required).is_some_and(|field| !field.is_null()),
            "{path}.{required} is required and is missing from {value}",
        );
    }

    for (name, child) in schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        // Absent is fine — `required` above is what makes a field mandatory.
        if let Some(field) = value.get(name).filter(|field| !field.is_null()) {
            check(child, field, &format!("{path}.{name}"));
        }
    }

    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, element) in array.iter().enumerate() {
            check(items, element, &format!("{path}[{index}]"));
        }
    }
}

/// Whether a value is of the JSON-schema type a field declares.
fn matches_type(expected: &str, value: &Value) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        _ => true,
    }
}

/// The fields a golden cannot pin down, replaced with a fixed marker.
///
/// A guid, a creation time and a signed token differ on every run; everything
/// else about a freshly created mission does not, and that is what the golden
/// is for.
const VOLATILE: &[&str] = &[
    "guid",
    "createTime",
    "lastEdited",
    "token",
    "submissionTime",
];

/// Replaces the volatile fields of a payload, recursively.
pub fn normalise(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| {
                    let replaced = match VOLATILE.contains(&name.as_str()) {
                        true => Value::String(format!("<{name}>")),
                        false => normalise(field),
                    };

                    (name.clone(), replaced)
                })
                .collect(),
        ),
        Value::Array(elements) => Value::Array(elements.iter().map(normalise).collect()),
        other => other.clone(),
    }
}
