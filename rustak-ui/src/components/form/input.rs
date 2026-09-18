//! The controls somebody types into.

use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

/// Reads the value out of an input event's target, whichever element it was.
fn input_value(event: &InputEvent) -> Option<String> {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .or_else(|| {
            event
                .target_dyn_into::<HtmlTextAreaElement>()
                .map(|area| area.value())
        })
}

/// The same, for a blur event.
fn blurred_value(event: &FocusEvent) -> Option<String> {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .or_else(|| {
            event
                .target_dyn_into::<HtmlTextAreaElement>()
                .map(|area| area.value())
        })
}

#[derive(Properties, PartialEq)]
pub struct TextInputProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub onblur: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,

    /// What the browser may offer to fill in. `username webauthn` is what tells
    /// a password manager that this field names the account a passkey is for.
    #[prop_or_default]
    pub autocomplete: Option<AttrValue>,
}

#[function_component(TextInput)]
pub fn text_input(props: &TextInputProps) -> Html {
    let oninput = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(value) = input_value(&event) {
                onchange.emit(value);
            }
        })
    };

    let onblur = {
        let blur = props.onblur.clone();
        Callback::from(move |event: FocusEvent| {
            if let Some(value) = blurred_value(&event) {
                blur.emit(value);
            }
        })
    };

    html! {
        <input
            id={props.id.clone()}
            class={classes!("field__input", props.invalid.then_some("field__input--invalid"))}
            type="text"
            value={props.value.clone()}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            autocomplete={props.autocomplete.clone()}
            {oninput}
            {onblur}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct TextAreaProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or(3)]
    pub rows: u32,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,

    /// Renders in a monospaced face, for values whose alignment carries meaning.
    #[prop_or_default]
    pub monospace: bool,
}

#[function_component(TextArea)]
pub fn text_area(props: &TextAreaProps) -> Html {
    let oninput = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(value) = input_value(&event) {
                onchange.emit(value);
            }
        })
    };

    html! {
        <textarea
            id={props.id.clone()}
            class={classes!(
                "field__input",
                "field__textarea",
                props.monospace.then_some("field__textarea--mono"),
                props.invalid.then_some("field__input--invalid"),
            )}
            rows={props.rows.to_string()}
            value={props.value.clone()}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            {oninput}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct NumberInputProps {
    pub id: AttrValue,

    /// Absent means the field is empty, which is not the same as zero.
    pub value: Option<i64>,
    pub onchange: Callback<Option<i64>>,

    #[prop_or_default]
    pub min: Option<i64>,

    #[prop_or_default]
    pub max: Option<i64>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

#[function_component(NumberInput)]
pub fn number_input(props: &NumberInputProps) -> Html {
    let oninput = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            let Some(raw) = input_value(&event) else {
                return;
            };

            // An empty box means "unset" rather than zero, and a half-typed
            // value such as "-" is left alone so the field does not fight
            // somebody mid-keystroke.
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                onchange.emit(None);
            } else if let Ok(parsed) = trimmed.parse::<i64>() {
                onchange.emit(Some(parsed));
            }
        })
    };

    html! {
        <input
            id={props.id.clone()}
            class={classes!("field__input", props.invalid.then_some("field__input--invalid"))}
            type="number"
            value={props.value.map(|value| value.to_string()).unwrap_or_default()}
            min={props.min.map(|value| value.to_string())}
            max={props.max.map(|value| value.to_string())}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            {oninput}
        />
    }
}
