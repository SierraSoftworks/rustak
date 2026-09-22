//! One node of a configuration form, drawn from one node of a JSON Schema.
//!
//! [`SchemaNode`] is recursive: an object draws a node per property, a tagged
//! union draws a picker and then the chosen variant's properties, a list draws
//! a node per item — see [`super::groups`] — and everything else is a single
//! labelled input, which is this file.
//!
//! # A value that is not there is `None`, not `null`
//!
//! Every node reports `Option<Value>`. Clearing an optional field removes its
//! key from the parent object rather than storing `null` or `""`, because the
//! plugin reads the document with serde and an absent key is what takes its
//! `#[serde(default)]`. A *required* text field reports `""` instead, so that
//! the server's schema check has something to object to by name.

use std::rc::Rc;

use rustak_api::ConfigIssue;
use serde_json::Value;
use yew::prelude::*;

use crate::components::{Field, NumberInput, SecretInput, Select, SelectOption, Switch, TextInput};

use super::groups;
use super::inputs::{DecimalInput, JsonField};
use super::schema::{self, Kind};

#[derive(Properties, PartialEq)]
pub struct SchemaNodeProps {
    /// The whole schema, which `$ref`s are resolved against.
    pub root: Rc<Value>,

    /// This node's own schema.
    pub schema: Value,

    /// Where this node's value sits in the document, as a JSON Pointer. It is
    /// what ties an issue the server reports to the input it is about.
    pub pointer: AttrValue,

    pub label: AttrValue,

    #[prop_or_default]
    pub required: bool,

    pub value: Option<Value>,
    pub onchange: Callback<Option<Value>>,

    pub issues: Rc<Vec<ConfigIssue>>,

    #[prop_or_default]
    pub disabled: bool,
}

impl SchemaNodeProps {
    /// `service-config-area-lat`, for `/area/lat`.
    pub fn id(&self) -> AttrValue {
        format!("service-config{}", self.pointer.replace('/', "-")).into()
    }

    /// The schema's `description`: a plugin's doc comment, as help text.
    ///
    /// Its first paragraph only, unwrapped. `schemars` carries the whole doc
    /// comment across, and what follows the summary line is written for
    /// somebody reading the source — examples, code blocks — not a form.
    pub fn help(&self) -> Option<AttrValue> {
        let text = schema::keyword(&self.root, &self.schema, "description")?.as_str()?;
        let summary = text.split("\n\n").next().unwrap_or_default();

        Some(summary.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|summary| !summary.is_empty())
            .map(AttrValue::from)
    }

    /// What the last validation said about exactly this value.
    pub fn error(&self) -> Option<AttrValue> {
        self.issues
            .iter()
            .find(|issue| issue.path.as_deref().unwrap_or_default() == self.pointer.as_str())
            .map(|issue| AttrValue::from(issue.message.clone()))
    }

    fn bound(&self, keyword: &str) -> Option<i64> {
        let bound = schema::keyword(&self.root, &self.schema, keyword)?;

        bound
            .as_i64()
            .or_else(|| bound.as_f64().map(|bound| bound as i64))
    }
}

#[function_component(SchemaNode)]
pub fn schema_node(props: &SchemaNodeProps) -> Html {
    match schema::kind(&props.root, &props.schema) {
        Kind::Object(fields) => groups::object(props, &fields),
        Kind::Variants(variants) => groups::variants(props, &variants),
        Kind::List(items) => groups::list(props, items),
        leaf => html! {
            <Field
                label={props.label.clone()}
                id={props.id()}
                required={props.required}
                help={props.help()}
                error={props.error()}
            >
                { input(props, &leaf) }
            </Field>
        },
    }
}

/// The single input a leaf wants.
fn input(props: &SchemaNodeProps, kind: &Kind<'_>) -> Html {
    let (id, disabled, invalid) = (props.id(), props.disabled, props.error().is_some());
    let value = props.value.as_ref();

    match kind {
        Kind::Text { secret } => {
            let required = props.required;
            let text = AttrValue::from(
                value
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
            let onchange = props.onchange.reform(move |text: String| {
                (required || !text.is_empty()).then_some(Value::String(text))
            });

            match secret {
                true => html! { <SecretInput {id} value={text} {onchange} {disabled} {invalid} /> },
                false => html! { <TextInput {id} value={text} {onchange} {disabled} {invalid} /> },
            }
        }
        Kind::Integer => html! {
            <NumberInput
                {id}
                value={value.and_then(Value::as_i64)}
                min={props.bound("minimum")}
                max={props.bound("maximum")}
                onchange={props.onchange.reform(|number: Option<i64>| number.map(Value::from))}
                {disabled}
                {invalid}
            />
        },
        Kind::Number => html! {
            <DecimalInput
                {id}
                value={value.and_then(Value::as_f64)}
                onchange={props.onchange.reform(|number: Option<f64>| {
                    number.and_then(serde_json::Number::from_f64).map(Value::Number)
                })}
                {disabled}
                {invalid}
            />
        },
        Kind::Boolean => html! {
            <Switch
                {id}
                checked={value.and_then(Value::as_bool).unwrap_or_default()}
                onchange={props.onchange.reform(|on: bool| Some(Value::Bool(on)))}
                {disabled}
            />
        },
        Kind::Choice(values) => {
            // Picked by position, so that a choice between numbers stays a
            // number on its way back out of a `<select>`.
            let offered: Vec<Value> = values.iter().map(|value| (*value).clone()).collect();
            let options: Vec<SelectOption> = offered
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let label = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_string);

                    SelectOption::new(index.to_string(), label)
                })
                .collect();
            let chosen = value
                .and_then(|value| offered.iter().position(|offered| offered == value))
                .map(|index| AttrValue::from(index.to_string()));
            let onchange = props.onchange.reform(move |picked: Option<String>| {
                offered.get(picked?.parse::<usize>().ok()?).cloned()
            });

            html! {
                <Select
                    {id}
                    value={chosen}
                    {options}
                    {onchange}
                    clearable={!props.required}
                    placeholder={if props.required { "Choose one" } else { "Not set" }}
                    {disabled}
                    {invalid}
                />
            }
        }
        _ => html! {
            <JsonField {id} value={props.value.clone()} onchange={props.onchange.clone()} {disabled} />
        },
    }
}
