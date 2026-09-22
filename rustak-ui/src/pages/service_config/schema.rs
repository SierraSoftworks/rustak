//! Reading a JSON Schema well enough to draw a form from it.
//!
//! Not a validator — the server holds every write to the schema, with a real
//! one. This is the other direction: given the schema a plugin registered,
//! which input does each part of its configuration want? It understands the
//! shapes `schemars` derives from a Rust settings struct, because that is where
//! nearly every schema here comes from:
//!
//! | In Rust | In the schema | Here |
//! |---|---|---|
//! | a struct | `type: object` + `properties` | [`Kind::Object`] |
//! | `#[serde(tag = "…")] enum` | `oneOf` of objects sharing a `const` property | [`Kind::Variants`] |
//! | a unit-variant `enum` | `enum`, or `oneOf` of `const`s | [`Kind::Choice`] |
//! | `Option<T>` | `anyOf: [T, null]` or `type: [T, "null"]` | the `T`, not required |
//! | a nested type | `$ref: "#/$defs/…"` | followed |
//! | `Vec<T>` | `type: array` + `items` | [`Kind::List`] |
//!
//! Anything else is [`Kind::Unknown`], which the form draws as a JSON box for
//! that one value: a schema this cannot read costs a nice input, never the
//! ability to edit.

use serde_json::{Map, Value};

/// How deep anything here goes before giving up: `$ref`s followed from one
/// node, levels of defaults built, and — in the form — levels drawn. A schema
/// may contain itself, and one that *requires* itself has no last level.
pub const MAX_DEPTH: usize = 16;

/// What kind of input a schema node wants.
#[derive(Debug, PartialEq)]
pub enum Kind<'a> {
    Object(Vec<Property<'a>>),
    Variants(Vec<Variant<'a>>),
    /// One of a fixed set of scalar values.
    Choice(Vec<&'a Value>),
    Text {
        secret: bool,
    },
    Integer,
    Number,
    Boolean,
    /// A list whose items all follow one schema.
    List(&'a Value),
    Unknown,
}

#[derive(Debug, PartialEq)]
pub struct Property<'a> {
    pub key: &'a str,
    pub schema: &'a Value,
    pub required: bool,
}

/// One alternative of a tagged union.
#[derive(Debug, PartialEq)]
pub struct Variant<'a> {
    pub label: String,
    pub schema: &'a Value,
    /// The property whose constant value says which variant an object is.
    pub tag: Option<(&'a str, &'a Value)>,
}

impl Variant<'_> {
    /// This variant's own fields: everything but the tag, which the variant
    /// picker sets.
    pub fn fields(&self) -> Vec<Property<'_>> {
        properties(self.schema)
            .into_iter()
            .filter(|property| self.tag.is_none_or(|(tag, _)| tag != property.key))
            .collect()
    }
}

/// Follows local `$ref`s, and the single-entry `allOf` older generators wrap
/// one in to hang a description on it.
pub fn resolve<'a>(root: &'a Value, mut node: &'a Value) -> &'a Value {
    for _ in 0..MAX_DEPTH {
        let next = match (node.get("$ref"), node.get("allOf")) {
            (Some(Value::String(target)), _) => target
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer)),
            (None, Some(Value::Array(all))) if all.len() == 1 => all.first(),
            _ => None,
        };

        match next {
            Some(next) => node = next,
            None => break,
        }
    }

    node
}

/// `node`, resolved, with "or null" taken off: whether a value may be absent is
/// the parent's `required`, not something the input needs to draw.
fn unwrapped<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    let node = resolve(root, node);
    let is_null = |entry: &Value| entry.get("type").and_then(Value::as_str) == Some("null");

    match alternatives(node) {
        Some([only, null]) | Some([null, only]) if is_null(null) && !is_null(only) => {
            resolve(root, only)
        }
        _ => node,
    }
}

fn alternatives(node: &Value) -> Option<&[Value]> {
    node.get("oneOf")
        .or_else(|| node.get("anyOf"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

/// The one non-null type a node names, if it names one.
fn type_of(node: &Value) -> Option<&str> {
    match node.get("type")? {
        Value::String(name) => Some(name),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .find(|name| *name != "null"),
        _ => None,
    }
}

fn properties(node: &Value) -> Vec<Property<'_>> {
    let required: Vec<&str> = node
        .get("required")
        .and_then(Value::as_array)
        .map(|keys| keys.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    node.get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(key, schema)| Property {
                    key,
                    schema,
                    required: required.contains(&key.as_str()),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The variants of a tagged union, when every alternative is an object.
fn variants<'a>(root: &'a Value, entries: &'a [Value]) -> Option<Vec<Variant<'a>>> {
    let resolved: Vec<&Value> = entries.iter().map(|entry| resolve(root, entry)).collect();

    if resolved.is_empty()
        || resolved
            .iter()
            .any(|entry| type_of(entry) != Some("object"))
    {
        return None;
    }

    // The tag is the property every alternative pins to a constant.
    let tag = properties(resolved[0])
        .into_iter()
        .map(|p| p.key)
        .find(|key| {
            resolved
                .iter()
                .all(|entry| entry.pointer(&format!("/properties/{key}/const")).is_some())
        });

    Some(
        resolved
            .into_iter()
            .enumerate()
            .map(|(index, schema)| {
                let tag = tag.and_then(|key| {
                    Some((key, schema.pointer(&format!("/properties/{key}/const"))?))
                });
                let label = schema
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| tag.and_then(|(_, value)| value.as_str()).map(humanise))
                    .unwrap_or_else(|| format!("Option {}", index + 1));

                Variant { label, schema, tag }
            })
            .collect(),
    )
}

/// What kind of input `node` wants.
pub fn kind<'a>(root: &'a Value, node: &'a Value) -> Kind<'a> {
    let node = unwrapped(root, node);

    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        return Kind::Choice(values.iter().filter(|value| !value.is_null()).collect());
    }

    if let Some(entries) = alternatives(node) {
        let constants: Vec<&Value> = entries.iter().filter_map(|e| e.get("const")).collect();

        return match variants(root, entries) {
            _ if !entries.is_empty() && constants.len() == entries.len() => Kind::Choice(constants),
            Some(variants) => Kind::Variants(variants),
            None => Kind::Unknown,
        };
    }

    match type_of(node) {
        Some("object") if node.get("properties").is_some() => Kind::Object(properties(node)),
        Some("string") => Kind::Text {
            secret: node.get("format").and_then(Value::as_str) == Some("password")
                || node.get("writeOnly") == Some(&Value::Bool(true)),
        },
        Some("integer") => Kind::Integer,
        Some("number") => Kind::Number,
        Some("boolean") => Kind::Boolean,
        Some("array") => node.get("items").map_or(Kind::Unknown, Kind::List),
        _ => Kind::Unknown,
    }
}

/// A keyword of `node`, looked for on the node itself before what it refers
/// to: a `description` beside a `$ref` is about this use of the type.
pub fn keyword<'a>(root: &'a Value, node: &'a Value, name: &str) -> Option<&'a Value> {
    node.get(name).or_else(|| unwrapped(root, node).get(name))
}

/// The value a newly added field starts with: the schema's own `default` where
/// it has one, and otherwise the emptiest thing of the right shape.
pub fn default_for(root: &Value, node: &Value) -> Value {
    default_at(root, node, 0)
}

fn default_at(root: &Value, node: &Value, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::Null;
    }

    if let Some(given) = keyword(root, node, "default").or_else(|| keyword(root, node, "const")) {
        return given.clone();
    }

    match kind(root, node) {
        Kind::Object(properties) => Value::Object(required_defaults(root, &properties, depth + 1)),
        Kind::Variants(variants) => variants.first().map_or(Value::Null, |first| {
            default_at(root, first.schema, depth + 1)
        }),
        Kind::Choice(values) => values.first().copied().cloned().unwrap_or(Value::Null),
        Kind::Text { .. } => Value::String(String::new()),
        Kind::Integer | Kind::Number => keyword(root, node, "minimum")
            .cloned()
            .unwrap_or_else(|| Value::from(0)),
        Kind::Boolean => Value::Bool(false),
        Kind::List(_) => Value::Array(Vec::new()),
        Kind::Unknown => Value::Null,
    }
}

fn required_defaults(
    root: &Value,
    properties: &[Property<'_>],
    depth: usize,
) -> Map<String, Value> {
    properties
        .iter()
        .filter(|property| property.required)
        .map(|property| {
            (
                property.key.to_string(),
                default_at(root, property.schema, depth),
            )
        })
        .collect()
}

/// Which variant `value` is: by its tag, or failing that by its keys.
pub fn variant_of(variants: &[Variant<'_>], value: &Value) -> Option<usize> {
    variants.iter().position(|variant| match variant.tag {
        Some((key, constant)) => value.get(key) == Some(constant),
        None => properties(variant.schema)
            .iter()
            .filter(|property| property.required)
            .all(|property| value.get(property.key).is_some()),
    })
}

/// A fresh value of `variant`, keeping whatever `previous` held under the same
/// name and type — so that switching a circle to a box and back does not cost
/// somebody the coordinates they had typed.
pub fn switched(root: &Value, variant: &Variant<'_>, previous: Option<&Value>) -> Value {
    let mut fresh = default_for(root, variant.schema);

    if let (Some(fresh), Some(previous)) =
        (fresh.as_object_mut(), previous.and_then(Value::as_object))
    {
        for property in variant.fields() {
            if let Some(kept) = previous.get(property.key)
                && fresh
                    .get(property.key)
                    .is_none_or(|new| std::mem::discriminant(new) == std::mem::discriminant(kept))
                && fresh.contains_key(property.key)
            {
                fresh.insert(property.key.to_string(), kept.clone());
            }
        }
    }

    fresh
}

/// `radius_km` as a label: "Radius km".
pub fn humanise(key: &str) -> String {
    let spaced = key.replace(['_', '-'], " ");
    let mut letters = spaced.chars();

    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => spaced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What `schemars` derives for a feed plugin's configuration: an optional,
    /// internally tagged `Area` behind a `$ref`.
    fn feed() -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {
                    "description": "Where the feed is looking.",
                    "anyOf": [{ "$ref": "#/$defs/Area" }, { "type": "null" }],
                },
                "api_key": { "type": "string", "format": "password" },
                "days": { "type": "integer", "minimum": 1, "maximum": 5 },
                "sensors": { "type": "array", "items": { "enum": ["modis", "viirs"] } },
                "labels": { "type": "object", "additionalProperties": { "type": "string" } },
            },
            "required": ["api_key"],
            "$defs": { "Area": { "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "const": "bbox" },
                        "south": { "type": "number" }, "north": { "type": "number" },
                    },
                    "required": ["kind", "south", "north"],
                },
                {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "const": "circle" },
                        "lat": { "type": "number", "minimum": -90 },
                        "radius_km": { "type": "number", "default": 50.0 },
                    },
                    "required": ["kind", "lat", "radius_km"],
                },
            ] } },
        })
    }

    #[test]
    fn each_shape_a_settings_struct_derives_gets_the_input_it_wants() {
        let root = feed();
        let property = |key: &str| &root["properties"][key];

        assert!(matches!(kind(&root, &root), Kind::Object(fields) if fields.len() == 5));
        assert_eq!(
            kind(&root, property("api_key")),
            Kind::Text { secret: true }
        );
        assert_eq!(kind(&root, property("days")), Kind::Integer);
        assert!(matches!(kind(&root, property("sensors")), Kind::List(_)));
        assert_eq!(
            kind(&root, property("labels")),
            Kind::Unknown,
            "a map is a JSON box"
        );

        let Kind::Variants(variants) = kind(&root, property("area")) else {
            panic!("an optional tagged enum behind a $ref is still a tagged enum");
        };
        assert_eq!(
            variants
                .iter()
                .map(|v| v.label.as_str())
                .collect::<Vec<_>>(),
            ["Bbox", "Circle"],
        );
        assert_eq!(
            variants[1]
                .fields()
                .iter()
                .map(|p| p.key)
                .collect::<Vec<_>>(),
            ["lat", "radius_km"],
            "the tag is the picker's, not a field",
        );
        assert_eq!(
            keyword(&root, property("area"), "description"),
            Some(&json!("Where the feed is looking.")),
        );
    }

    #[test]
    fn a_new_value_starts_from_the_schemas_defaults_and_keeps_what_carries_over() {
        let root = feed();
        let Kind::Variants(variants) = kind(&root, &root["properties"]["area"]) else {
            panic!("variants");
        };

        assert_eq!(default_for(&root, &root), json!({ "api_key": "" }));

        let circle = switched(&root, &variants[1], None);
        assert_eq!(
            circle,
            json!({ "kind": "circle", "lat": -90, "radius_km": 50.0 })
        );
        assert_eq!(variant_of(&variants, &circle), Some(1));

        let moved = json!({ "kind": "circle", "lat": 51.5, "radius_km": 120.0 });
        assert_eq!(
            switched(&root, &variants[1], Some(&moved)),
            moved,
            "re-picking the same variant loses nothing",
        );
        assert_eq!(variant_of(&variants, &json!({ "kind": "hexagon" })), None);
    }

    #[test]
    fn a_schema_that_refers_to_itself_is_not_followed_forever() {
        let root = json!({ "$ref": "#" });

        assert_eq!(kind(&root, &root), Kind::Unknown);
        assert_eq!(humanise("radius_km"), "Radius km");
    }

    #[test]
    fn a_schema_that_requires_itself_still_has_a_default() {
        let root = json!({
            "type": "object",
            "properties": { "child": { "$ref": "#" } },
            "required": ["child"],
        });

        // Whatever it holds, it is finite: the point is that this returns.
        assert!(default_for(&root, &root).is_object());
    }
}
