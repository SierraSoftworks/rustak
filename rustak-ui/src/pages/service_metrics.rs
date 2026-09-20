//! A heartbeat's `metrics`, rendered as something an operator can read.
//!
//! `metrics` is untyped on purpose — a plugin knows what is worth counting and
//! `rustak-api` does not — so this page cannot lay it out from a schema. What
//! it can do is refuse to show anybody a wall of JSON: an object becomes a
//! key/value table, a nested object becomes an indented block under its key,
//! and an array becomes the comma-joined list it reads as in a sentence.
//!
//! Two rules matter more than the layout:
//!
//! * **Only a value that is not an object is ever shown verbatim.** A plugin
//!   that reports a bare number or a string gets a line of text, because there
//!   is nothing to tabulate; everything else is flattened into rows first.
//! * **A string is text, never markup.** Every value here came off the wire
//!   from a sidecar, and each one reaches the DOM through Yew's `{value}`
//!   interpolation, which escapes it. Nothing on this path builds HTML from a
//!   metric.

use serde_json::Value;
use yew::prelude::*;

/// What is shown for a value that is not there: a null, an empty object, an
/// empty array. One spelling, so "nothing reported" looks the same everywhere.
const NOTHING: &str = "—";

/// One line of the metrics table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricRow {
    /// The key, exactly as the plugin spelled it.
    pub key: String,

    /// The value, already flattened to text. Empty on the heading row that a
    /// nested object gets, which has its children beneath it instead.
    pub value: String,

    /// How far in the row sits: `0` for a top-level key, one more for each
    /// level of nesting.
    pub depth: usize,

    /// Whether this row heads a nested object rather than carrying a value.
    pub group: bool,
}

/// What a heartbeat's metrics can be shown as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metrics {
    /// The object a well-behaved plugin reports, flattened into rows.
    Table(Vec<MetricRow>),

    /// Anything that is not an object: shown as the text it is, because there
    /// are no keys to put in a table.
    Raw(String),
}

/// Reads a heartbeat's metrics, or [`None`] when the plugin reported none.
pub fn read_metrics(value: &Value) -> Option<Metrics> {
    match value {
        Value::Null => None,
        Value::Object(fields) if fields.is_empty() => None,
        Value::Object(_) => {
            let mut rows = Vec::new();
            flatten(value, 0, &mut rows);
            Some(Metrics::Table(rows))
        }
        other => Some(Metrics::Raw(scalar(other))),
    }
}

/// Appends `object`'s fields to `rows`, descending into the nested ones.
fn flatten(object: &Value, depth: usize, rows: &mut Vec<MetricRow>) {
    let Some(fields) = object.as_object() else {
        return;
    };

    for (key, value) in fields {
        match value {
            Value::Object(nested) if !nested.is_empty() => {
                rows.push(MetricRow {
                    key: key.clone(),
                    value: String::new(),
                    depth,
                    group: true,
                });
                flatten(value, depth + 1, rows);
            }
            _ => rows.push(MetricRow {
                key: key.clone(),
                value: scalar(value),
                depth,
                group: false,
            }),
        }
    }
}

/// One value as a line of text: a list joined with commas, a string as itself,
/// and an object — only reachable inside a list — as the compact JSON it is.
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => NOTHING.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.is_empty() => NOTHING.to_string(),
        Value::Array(items) => items.iter().map(scalar).collect::<Vec<_>>().join(", "),
        Value::Object(fields) if fields.is_empty() => NOTHING.to_string(),
        Value::Object(_) => value.to_string(),
    }
}

#[derive(Properties, PartialEq)]
pub struct MetricsTableProps {
    /// The `metrics` object as the service last reported it.
    pub metrics: Value,
}

/// The metrics table, or the line that says there is nothing in it yet.
#[function_component(MetricsTable)]
pub fn metrics_table(props: &MetricsTableProps) -> Html {
    match read_metrics(&props.metrics) {
        None => html! {
            <p class="panel-empty">
                { "This service has not reported any metrics. A heartbeat carries whatever the \
                   plugin counts; one that carries nothing is not a fault." }
            </p>
        },
        Some(Metrics::Raw(text)) => html! { <p class="metrics__raw">{ text }</p> },
        Some(Metrics::Table(rows)) => html! {
            <dl class="metrics">
                { for rows.into_iter().enumerate().map(|(index, row)| html! {
                    <div
                        key={index}
                        class={classes!(
                            "metrics__row",
                            (row.depth > 0).then_some("metrics__row--nested"),
                            row.group.then_some("metrics__row--group"),
                        )}
                    >
                        <dt>{ row.key }</dt>
                        <dd>{ row.value }</dd>
                    </div>
                }) }
            </dl>
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn table(value: &Value) -> Vec<MetricRow> {
        match read_metrics(value) {
            Some(Metrics::Table(rows)) => rows,
            other => panic!("expected a table, got {other:?}"),
        }
    }

    fn pairs(rows: &[MetricRow]) -> Vec<(String, String, usize)> {
        rows.iter()
            .map(|row| (row.key.clone(), row.value.clone(), row.depth))
            .collect()
    }

    #[test]
    fn a_flat_object_becomes_one_row_per_key() {
        let rows = table(&json!({ "offered": 412, "published": 388, "healthy": true }));

        // The rows come out in the map's own order, which is the key order
        // `serde_json`'s default `BTreeMap` imposes rather than the order the
        // plugin wrote them in — so they are compared as a set.
        let mut seen = pairs(&rows);
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("healthy".to_string(), "true".to_string(), 0),
                ("offered".to_string(), "412".to_string(), 0),
                ("published".to_string(), "388".to_string(), 0),
            ]
        );
        assert!(rows.iter().all(|row| !row.group));
    }

    #[test]
    fn a_nested_object_is_a_heading_with_its_fields_indented_under_it() {
        let rows = table(&json!({
            "source": { "kind": "aisstream", "state": "connected" },
        }));

        assert_eq!(rows[0].key, "source");
        assert!(rows[0].group, "the nested object heads its own block");
        assert_eq!(rows[0].value, "");
        assert_eq!(rows[0].depth, 0);

        let mut children = pairs(&rows[1..]);
        children.sort();
        assert_eq!(
            children,
            vec![
                ("kind".to_string(), "aisstream".to_string(), 1),
                ("state".to_string(), "connected".to_string(), 1),
            ]
        );
    }

    #[test]
    fn an_array_is_joined_with_commas_rather_than_shown_as_json() {
        let rows = table(&json!({
            "feeds": ["ais", "adsb", "weather"],
            "errors": [],
            "recent": [{ "at": 1 }],
        }));

        let by_key = |key: &str| {
            rows.iter()
                .find(|row| row.key == key)
                .map(|row| row.value.clone())
                .unwrap_or_default()
        };

        assert_eq!(by_key("feeds"), "ais, adsb, weather");
        assert_eq!(by_key("errors"), NOTHING);
        // An object inside a list has no row of its own to go in, so it is the
        // one place compact JSON is the honest answer.
        assert_eq!(by_key("recent"), r#"{"at":1}"#);
    }

    #[test]
    fn a_value_that_is_not_an_object_is_shown_as_the_text_it_is() {
        assert_eq!(read_metrics(&Value::Null), None);
        assert_eq!(read_metrics(&json!({})), None);
        assert_eq!(
            read_metrics(&json!(1204)),
            Some(Metrics::Raw("1204".to_string()))
        );
        assert_eq!(
            read_metrics(&json!(["ais", "adsb"])),
            Some(Metrics::Raw("ais, adsb".to_string()))
        );
        assert_eq!(
            read_metrics(&json!("the feed is quiet")),
            Some(Metrics::Raw("the feed is quiet".to_string()))
        );
    }

    #[test]
    fn a_string_that_looks_like_markup_stays_a_string() {
        // The page renders every one of these through Yew's `{value}`, which
        // escapes it. What this asserts is that nothing here *unwraps* the
        // string on the way — no trimming of tags, no re-encoding, no
        // `to_string()` that would add quotes and hide what was reported.
        let markup = "<img src=x onerror=alert(1)>";
        let rows = table(&json!({ "note": markup, "nested": { "note": markup } }));

        let values: Vec<&str> = rows.iter().map(|row| row.value.as_str()).collect();
        assert!(values.contains(&markup), "{values:?}");
        assert_eq!(values.iter().filter(|value| **value == markup).count(), 2);

        assert_eq!(
            read_metrics(&json!(markup)),
            Some(Metrics::Raw(markup.to_string()))
        );
    }
}
