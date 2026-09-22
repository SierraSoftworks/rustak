//! The parts of a configuration form that hold other parts: an object, a tagged
//! union, and a list. Each draws a `<fieldset>` and recurses into
//! [`SchemaNode`] for what is inside it.

use serde_json::{Map, Value};
use yew::prelude::*;

use crate::components::{Button, ButtonKind, Field, Select, SelectOption, Switch};

use super::form::{SchemaNode, SchemaNodeProps};
use super::schema::{self, Kind, Property, Variant};

/// The frame every group shares: a legend, the help text, and what is wrong
/// with the group as a whole — a relation between its fields, say.
fn group(props: &SchemaNodeProps, body: Html) -> Html {
    html! {
        <fieldset class="config-form__group" disabled={props.disabled}>
            <legend class="config-form__legend">{ &props.label }</legend>

            if let Some(help) = props.help() {
                <p class="field__help">{ help }</p>
            }
            if let Some(error) = props.error() {
                <p class="field__error" role="alert">{ error }</p>
            }

            { body }
        </fieldset>
    }
}

/// A node per property, each writing its own key of this node's object.
fn fields(props: &SchemaNodeProps, fields: &[Property<'_>]) -> Html {
    let current: Map<String, Value> = props
        .value
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    fields
        .iter()
        .map(|property| {
            let key = property.key.to_string();
            let label = schema::keyword(&props.root, property.schema, "title")
                .and_then(Value::as_str)
                .map_or_else(|| schema::humanise(&key), str::to_string);
            let onchange = {
                let (current, key, onchange) =
                    (current.clone(), key.clone(), props.onchange.clone());
                Callback::from(move |value: Option<Value>| {
                    let mut next = current.clone();

                    match value {
                        Some(value) => next.insert(key.clone(), value),
                        None => next.remove(&key),
                    };

                    onchange.emit(Some(Value::Object(next)));
                })
            };

            html! {
                <SchemaNode
                    key={key.clone()}
                    root={props.root.clone()}
                    schema={property.schema.clone()}
                    pointer={format!("{}/{key}", props.pointer)}
                    {label}
                    required={property.required}
                    value={current.get(&key).cloned()}
                    {onchange}
                    issues={props.issues.clone()}
                    disabled={props.disabled}
                />
            }
        })
        .collect()
}

pub fn object(props: &SchemaNodeProps, properties: &[Property<'_>]) -> Html {
    // The document itself: no frame, because the card around it is the frame.
    if props.pointer.is_empty() {
        return fields(props, properties);
    }

    let present = props.value.is_some();
    let toggle = {
        let (root, node, onchange) = (
            props.root.clone(),
            props.schema.clone(),
            props.onchange.clone(),
        );
        Callback::from(move |on: bool| onchange.emit(on.then(|| schema::default_for(&root, &node))))
    };

    group(
        props,
        html! {
            <>
                if !props.required {
                    <Switch
                        id={format!("{}-set", props.id())}
                        checked={present}
                        label="Set"
                        onchange={toggle}
                        disabled={props.disabled}
                    />
                }
                if present || props.required {
                    { fields(props, properties) }
                }
            </>
        },
    )
}

pub fn variants(props: &SchemaNodeProps, variants: &[Variant<'_>]) -> Html {
    let chosen = props
        .value
        .as_ref()
        .and_then(|value| schema::variant_of(variants, value));
    let options: Vec<SelectOption> = variants
        .iter()
        .enumerate()
        .map(|(index, variant)| SelectOption::new(index.to_string(), variant.label.clone()))
        .collect();

    // The closure outlives this render, so it reads the schema again from what
    // it owns rather than borrowing `variants`.
    let onchange = {
        let (root, node) = (props.root.clone(), props.schema.clone());
        let (previous, onchange) = (props.value.clone(), props.onchange.clone());
        Callback::from(move |picked: Option<String>| {
            let Kind::Variants(variants) = schema::kind(&root, &node) else {
                return;
            };

            onchange.emit(
                picked
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| variants.get(index))
                    .map(|variant| schema::switched(&root, variant, previous.as_ref())),
            );
        })
    };

    let picker = variants
        .first()
        .and_then(|variant| variant.tag)
        .map_or_else(|| "Type".to_string(), |(tag, _)| schema::humanise(tag));
    let id = AttrValue::from(format!("{}-variant", props.id()));

    group(
        props,
        html! {
            <>
                <Field label={picker} id={id.clone()} required={props.required}>
                    <Select
                        {id}
                        value={chosen.map(|index| AttrValue::from(index.to_string()))}
                        {options}
                        {onchange}
                        clearable={!props.required}
                        placeholder={if props.required { "Choose one" } else { "Not set" }}
                        disabled={props.disabled}
                    />
                </Field>

                if let Some(variant) = chosen.and_then(|index| variants.get(index)) {
                    { fields(props, &variant.fields()) }
                }
            </>
        },
    )
}

/// A callback that writes the list back once `change` has been applied to a
/// copy of it, whatever event it is answering.
fn rewriting<E: 'static>(
    props: &SchemaNodeProps,
    items: &[Value],
    change: impl Fn(&mut Vec<Value>, E) + 'static,
) -> Callback<E> {
    let (items, onchange) = (items.to_vec(), props.onchange.clone());

    Callback::from(move |event: E| {
        let mut next = items.clone();
        change(&mut next, event);
        onchange.emit(Some(Value::Array(next)));
    })
}

pub fn list(props: &SchemaNodeProps, item: &Value) -> Html {
    let items: Vec<Value> = props
        .value
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let add = {
        let fresh = schema::default_for(&props.root, item);
        rewriting(props, &items, move |items, _: MouseEvent| {
            items.push(fresh.clone());
        })
    };

    let rows = items.iter().enumerate().map(|(index, value)| {
        let remove = rewriting(props, &items, move |items, _: MouseEvent| {
            items.remove(index);
        });
        // An item that was cleared is still an item; removing one is what the
        // button beside it is for.
        let onchange = rewriting(props, &items, move |items, value: Option<Value>| {
            if let Some(value) = value {
                items[index] = value;
            }
        });

        html! {
            <div class="config-form__item" key={index}>
                <SchemaNode
                    root={props.root.clone()}
                    schema={item.clone()}
                    pointer={format!("{}/{index}", props.pointer)}
                    label={format!("Item {}", index + 1)}
                    required=true
                    value={value.clone()}
                    {onchange}
                    issues={props.issues.clone()}
                    disabled={props.disabled}
                />
                <Button
                    kind={ButtonKind::Subtle}
                    small=true
                    disabled={props.disabled}
                    onclick={remove}
                >
                    { "Remove" }
                </Button>
            </div>
        }
    });

    group(
        props,
        html! {
            <>
                { for rows }
                <Button small=true disabled={props.disabled} onclick={add}>{ "Add" }</Button>
            </>
        },
    )
}
