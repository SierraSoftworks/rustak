//! Holding a demo configuration to the schema its service registered.
//!
//! The server does this with a real validator, which the wasm bundle has no
//! business carrying. Without it the demo would store what production answers
//! `422` to, and a walkthrough would teach somebody the wrong thing. So this is
//! the small part of JSON Schema that `schemars` derives from a settings
//! struct — types, ranges, required and unknown keys, tagged unions — and, like
//! the server, it says *where* and *why* and never repeats the value.

use rustak_api::ConfigIssue;
use serde_json::Value;

/// A schema may contain itself; a document is finite, but this does not rely on
/// that.
const MAX_DEPTH: usize = 32;

const NOT_ALLOWED: &str = "This is not one of the values allowed here.";

/// What is wrong with `config` according to `schema`: the first problem found.
pub fn issues(schema: &Value, config: &Value) -> Vec<ConfigIssue> {
    check(schema, schema, config, "", 0)
        .err()
        .into_iter()
        .collect()
}

fn check(
    root: &Value,
    node: &Value,
    value: &Value,
    path: &str,
    depth: usize,
) -> Result<(), ConfigIssue> {
    if depth > MAX_DEPTH {
        return Ok(());
    }

    let fail = |message: &str| Err(ConfigIssue::at(path, message));
    let list = |keyword: &str| node.get(keyword).and_then(Value::as_array);
    let limit = |keyword: &str| node.get(keyword).and_then(Value::as_f64);

    let referred = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|target| root.pointer(target.strip_prefix('#')?));
    if let Some(target) = referred {
        check(root, target, value, path, depth + 1)?;
    }

    for keyword in ["anyOf", "oneOf"] {
        let alternatives = list(keyword).map(Vec::as_slice).unwrap_or_default();
        let failures: Vec<ConfigIssue> = alternatives
            .iter()
            .filter_map(|alternative| check(root, alternative, value, path, depth + 1).err())
            .collect();

        if !alternatives.is_empty() && failures.len() == alternatives.len() {
            return Err(closest(failures, path));
        }
    }

    if !type_allows(node, value) {
        return fail("This is not the kind of value expected here.");
    }
    if node.get("const").is_some_and(|only| only != value)
        || list("enum").is_some_and(|allowed| !allowed.contains(value))
    {
        return fail(NOT_ALLOWED);
    }

    if let Some(number) = value.as_f64() {
        if limit("minimum").is_some_and(|minimum| number < minimum) {
            return fail("This is below the smallest value allowed.");
        }
        if limit("maximum").is_some_and(|maximum| number > maximum) {
            return fail("This is above the largest value allowed.");
        }
    }

    if let Some(object) = value.as_object() {
        let known = node.get("properties").and_then(Value::as_object);
        let closed = node.get("additionalProperties") == Some(&Value::Bool(false));

        // Before `required`, so that the wrong variant of a tagged union fails
        // on its tag — which is how `closest` tells it from the right one.
        for (key, child) in object {
            let at = format!("{path}/{key}");

            match known.and_then(|known| known.get(key)) {
                Some(schema) => check(root, schema, child, &at, depth + 1)?,
                None if closed => {
                    return Err(ConfigIssue::at(
                        at,
                        "This is not a setting this service reads.",
                    ));
                }
                None => {}
            }
        }

        let missing = list("required")
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|key| !object.contains_key(*key));
        if let Some(missing) = missing {
            return Err(ConfigIssue::at(
                format!("{path}/{missing}"),
                "This is required.",
            ));
        }
    }

    if let (Some(items), Some(entries)) = (node.get("items"), value.as_array()) {
        for (index, entry) in entries.iter().enumerate() {
            check(root, items, entry, &format!("{path}/{index}"), depth + 1)?;
        }
    }

    Ok(())
}

fn type_allows(node: &Value, value: &Value) -> bool {
    let is = |name: &str| match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        _ => true,
    };

    match node.get("type") {
        Some(Value::String(name)) => is(name),
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).any(is),
        _ => true,
    }
}

/// Of the ways every alternative failed, the one worth showing: not a variant
/// that was never meant (it failed on its tag), and otherwise whichever got
/// furthest into the value before objecting.
fn closest(failures: Vec<ConfigIssue>, path: &str) -> ConfigIssue {
    failures
        .into_iter()
        .filter(|issue| issue.message != NOT_ALLOWED)
        .rev()
        .max_by_key(|issue| issue.path.as_deref().unwrap_or_default().len())
        .unwrap_or_else(|| ConfigIssue::at(path, NOT_ALLOWED))
}
