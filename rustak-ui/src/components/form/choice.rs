//! The controls somebody picks from.

use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

/// One choice in a [`Select`].
#[derive(Clone, PartialEq)]
pub struct SelectOption {
    pub value: AttrValue,
    pub label: AttrValue,
}

impl SelectOption {
    pub fn new(value: impl Into<AttrValue>, label: impl Into<AttrValue>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct SelectProps {
    pub id: AttrValue,

    /// Absent means nothing is chosen, which shows the placeholder.
    pub value: Option<AttrValue>,
    pub onchange: Callback<Option<String>>,

    pub options: Vec<SelectOption>,

    #[prop_or(AttrValue::from("Choose one"))]
    pub placeholder: AttrValue,

    /// Lets somebody return to having nothing chosen.
    #[prop_or_default]
    pub clearable: bool,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

/// A single-choice picker.
///
/// Built on the browser's own `<select>`. A bespoke dropdown would allow richer
/// entries, but this is keyboard-navigable, usable on a touch device and
/// accessible without any work on our part — none of which is worth trading away
/// until a picker genuinely needs to render something the browser cannot.
#[function_component(Select)]
pub fn select(props: &SelectProps) -> Html {
    let onchange = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: Event| {
            let Some(element) = event.target_dyn_into::<HtmlSelectElement>() else {
                return;
            };
            let value = element.value();
            onchange.emit(if value.is_empty() { None } else { Some(value) });
        })
    };

    // The current value may name something no longer offered. Showing it keeps
    // the field honest about what is stored, rather than silently appearing to
    // be set to the first entry.
    let missing = props
        .value
        .as_ref()
        .filter(|current| !props.options.iter().any(|option| &&option.value == current));

    html! {
        <select
            id={props.id.clone()}
            class={classes!(
                "field__input",
                "field__select",
                props.invalid.then_some("field__input--invalid"),
            )}
            value={props.value.clone()}
            disabled={props.disabled}
            {onchange}
        >
            <option value="" disabled={!props.clearable} selected={props.value.is_none()}>
                { &props.placeholder }
            </option>

            if let Some(missing) = missing {
                <option value={missing.clone()} selected=true>
                    { format!("{missing} (no longer available)") }
                </option>
            }

            { for props.options.iter().map(|option| html! {
                <option
                    key={option.value.as_str()}
                    value={option.value.clone()}
                    selected={props.value.as_ref() == Some(&option.value)}
                >
                    { &option.label }
                </option>
            }) }
        </select>
    }
}

#[derive(Properties, PartialEq)]
pub struct SwitchProps {
    pub id: AttrValue,
    pub checked: bool,
    pub onchange: Callback<bool>,

    /// Sits beside the switch, since a bare toggle says nothing about what it
    /// controls.
    #[prop_or_default]
    pub label: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,
}

/// A toggle, for the settings that are simply on or off.
#[function_component(Switch)]
pub fn switch(props: &SwitchProps) -> Html {
    let onclick = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: MouseEvent| {
            if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
                onchange.emit(input.checked());
            }
        })
    };

    html! {
        <label class={classes!("switch", props.disabled.then_some("switch--disabled"))}>
            <input
                id={props.id.clone()}
                class="switch__input"
                type="checkbox"
                checked={props.checked}
                disabled={props.disabled}
                {onclick}
            />
            <span class="switch__track" aria-hidden="true"><span class="switch__thumb" /></span>
            if let Some(label) = &props.label {
                <span class="switch__label">{ label }</span>
            }
        </label>
    }
}
