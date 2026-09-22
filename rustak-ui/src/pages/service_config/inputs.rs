//! The two inputs a schema-drawn form needs that the shared kit does not have.
//!
//! Both keep the text somebody typed as *text*, beside the value it parses to.
//! A controlled input that re-rendered from the parsed value would turn `51.`
//! back into `51` between two keystrokes, and a JSON box would reformat itself
//! under the cursor.

use serde_json::Value;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::components::TextArea;

fn shown(value: Option<f64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

#[derive(Properties, PartialEq)]
pub struct DecimalInputProps {
    pub id: AttrValue,

    /// Absent means the field is empty, which is not the same as zero.
    pub value: Option<f64>,
    pub onchange: Callback<Option<f64>>,

    /// The schema's `minimum` and `maximum`. A text input has no range of its
    /// own, so they are shown as the placeholder and a value outside them is
    /// marked before anybody saves.
    #[prop_or_default]
    pub min: Option<f64>,

    #[prop_or_default]
    pub max: Option<f64>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

/// A number with a fractional part: a latitude, a radius.
///
/// `type="text"` with a decimal keyboard rather than `type="number"`, because a
/// number input reports a half-typed `-` as the empty string, and an input that
/// is re-rendered from that loses the keystroke.
#[function_component(DecimalInput)]
pub fn decimal_input(props: &DecimalInputProps) -> Html {
    let text = use_state(|| shown(props.value));

    {
        // The value changed from outside — a reload, a variant switched — rather
        // than by typing here, which is the one time the text should follow it.
        let text = text.clone();
        use_effect_with(props.value, move |value| {
            if text.trim().parse::<f64>().ok() != *value {
                text.set(shown(*value));
            }
            || ()
        });
    }

    let oninput = {
        let (text, onchange) = (text.clone(), props.onchange.clone());
        Callback::from(move |event: InputEvent| {
            let Some(input) = event.target_dyn_into::<HtmlInputElement>() else {
                return;
            };
            let raw = input.value();

            match raw.trim().parse::<f64>() {
                _ if raw.trim().is_empty() => onchange.emit(None),
                Ok(parsed) if parsed.is_finite() => onchange.emit(Some(parsed)),
                // Half-typed. Left alone until it is a number.
                _ => {}
            }

            text.set(raw);
        })
    };

    let outside = props.value.is_some_and(|value| {
        props.min.is_some_and(|min| value < min) || props.max.is_some_and(|max| value > max)
    });
    let range = match (props.min, props.max) {
        (Some(min), Some(max)) => Some(format!("Between {min} and {max}")),
        (Some(min), None) => Some(format!("At least {min}")),
        (None, Some(max)) => Some(format!("At most {max}")),
        (None, None) => None,
    }
    .map(AttrValue::from);
    let invalid = props.invalid || outside;

    html! {
        <input
            id={props.id.clone()}
            class={classes!("field__input", invalid.then_some("field__input--invalid"))}
            type="text"
            inputmode="decimal"
            aria-invalid={invalid.then_some("true")}
            placeholder={range.clone()}
            title={range}
            value={(*text).clone()}
            disabled={props.disabled}
            {oninput}
        />
    }
}

fn pretty(value: Option<&Value>) -> String {
    value
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
        .unwrap_or_default()
}

#[derive(Properties, PartialEq)]
pub struct JsonFieldProps {
    pub id: AttrValue,
    pub value: Option<Value>,
    pub onchange: Callback<Option<Value>>,

    #[prop_or_default]
    pub disabled: bool,
}

/// One value edited as JSON, for a part of a schema the form cannot draw.
///
/// What keeps an unusual schema editable: the rest of the form stays a form,
/// and this one value is a box that reports only what parses.
#[function_component(JsonField)]
pub fn json_field(props: &JsonFieldProps) -> Html {
    let text = use_state(|| pretty(props.value.as_ref()));

    {
        let text = text.clone();
        use_effect_with(props.value.clone(), move |value| {
            if serde_json::from_str::<Value>(&text).ok() != *value {
                text.set(pretty(value.as_ref()));
            }
            || ()
        });
    }

    let onchange = {
        let (text, onchange) = (text.clone(), props.onchange.clone());
        Callback::from(move |raw: String| {
            match serde_json::from_str::<Value>(&raw) {
                _ if raw.trim().is_empty() => onchange.emit(None),
                Ok(parsed) => onchange.emit(Some(parsed)),
                Err(_) => {}
            }

            text.set(raw);
        })
    };

    let unreadable = !text.trim().is_empty() && serde_json::from_str::<Value>(&text).is_err();

    html! {
        <TextArea
            id={props.id.clone()}
            value={(*text).clone()}
            rows={4}
            monospace=true
            invalid={unreadable}
            disabled={props.disabled}
            {onchange}
        />
    }
}
