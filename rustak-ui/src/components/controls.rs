//! Buttons and form controls, as yewi draws them.
//!
//! Each of these takes what a page has to hand — an id, a value, a callback
//! with the new value — and renders the yewi control for it, so a page states
//! what it needs and the look lives in one place. Ids stay on the focusable
//! element, which is what a `<label for>` and a test both point at.

use std::rc::Rc;

use yew::prelude::*;
use yewi::Attrs;

use super::buttons::size_of;

#[derive(Properties, PartialEq)]
pub struct FieldProps {
    pub label: AttrValue,

    /// The id of the control inside, which the label points at.
    pub id: AttrValue,

    #[prop_or_default]
    pub required: bool,

    #[prop_or_default]
    pub help: Option<AttrValue>,

    /// Shown instead of the help text, because an error is the more urgent of
    /// the two and stacking them makes the next field jump as somebody types.
    #[prop_or_default]
    pub error: Option<AttrValue>,

    pub children: Html,
}

/// A label, a control, and either help text or an error.
#[function_component(Field)]
pub fn field(props: &FieldProps) -> Html {
    html! {
        <yewi::Field
            label={html! { props.label.clone() }}
            control_id={props.id.clone()}
            required={props.required}
            hint={props.help.clone().map(Html::from).unwrap_or_default()}
            error={props.error.clone().map(Html::from).unwrap_or_default()}
        >
            { props.children.clone() }
        </yewi::Field>
    }
}

#[derive(Properties, PartialEq)]
pub struct TextInputProps {
    pub id: AttrValue,
    pub value: AttrValue,

    /// Fired with the new text on every edit.
    pub onchange: Callback<String>,

    /// Fired with the text as it stands when focus leaves.
    #[prop_or_default]
    pub onblur: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,

    #[prop_or_default]
    pub autocomplete: Option<AttrValue>,

    /// The id of a `<datalist>` of suggestions.
    #[prop_or_default]
    pub list: Option<AttrValue>,

    /// Names the input where no [`Field`] labels it.
    #[prop_or_default]
    pub label: Option<AttrValue>,

    #[prop_or_default]
    pub small: bool,
}

#[function_component(TextInput)]
pub fn text_input(props: &TextInputProps) -> Html {
    let extra = [("list", &props.list), ("aria-label", &props.label)]
        .into_iter()
        .filter_map(|(name, value)| value.clone().map(|value| (name, value)));

    html! {
        <yewi::TextInput
            id={props.id.clone()}
            value={props.value.clone()}
            on_input={props.onchange.clone()}
            on_blur_value={props.onblur.clone()}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            invalid={props.invalid}
            size={size_of(props.small, false)}
            autocomplete={props.autocomplete.clone()}
            attrs={Attrs::new(extra)}
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

    #[prop_or_default]
    pub monospace: bool,
}

#[function_component(TextArea)]
pub fn text_area(props: &TextAreaProps) -> Html {
    html! {
        <yewi::TextArea
            id={props.id.clone()}
            value={props.value.clone()}
            on_input={props.onchange.clone()}
            placeholder={props.placeholder.clone()}
            rows={props.rows}
            disabled={props.disabled}
            invalid={props.invalid}
            mono={props.monospace}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct NumberInputProps {
    pub id: AttrValue,
    pub value: Option<i64>,

    /// Fired with the new number, or `None` once the box is empty.
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

/// A whole number. What is typed is kept whole here rather than by every
/// caller: a fraction is rounded, since nothing this console sets has one.
#[function_component(NumberInput)]
pub fn number_input(props: &NumberInputProps) -> Html {
    let on_input = props
        .onchange
        .reform(|value: Option<f64>| value.map(|number| number.round() as i64));

    html! {
        <yewi::NumberInput
            id={props.id.clone()}
            value={props.value.map(|number| number as f64)}
            {on_input}
            min={props.min.map(|number| number as f64)}
            max={props.max.map(|number| number as f64)}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            invalid={props.invalid}
        />
    }
}

/// One choice a [`Select`] offers.
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
    pub value: Option<AttrValue>,

    /// Fired with the chosen value, or `None` when the choice is cleared.
    pub onchange: Callback<Option<String>>,
    pub options: Vec<SelectOption>,

    #[prop_or(AttrValue::from("Choose one"))]
    pub placeholder: AttrValue,

    /// Whether "nothing" can be chosen again once something has been.
    #[prop_or_default]
    pub clearable: bool,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,

    /// Names the select where no [`Field`] labels it.
    #[prop_or_default]
    pub label: Option<AttrValue>,
}

/// One choice from a short list: the browser's own select, so a phone shows
/// its own picker and a keyboard types ahead.
#[function_component(Select)]
pub fn select(props: &SelectProps) -> Html {
    let options: Rc<[(AttrValue, AttrValue)]> = props
        .options
        .iter()
        .map(|option| (option.value.clone(), option.label.clone()))
        .collect();
    let on_change = props
        .onchange
        .reform(|value: String| (!value.is_empty()).then_some(value));

    html! {
        <yewi::NativeSelect
            id={props.id.clone()}
            {options}
            value={props.value.clone().unwrap_or_default()}
            {on_change}
            placeholder={props.placeholder.clone()}
            clearable={props.clearable}
            disabled={props.disabled}
            invalid={props.invalid}
            label={props.label.clone()}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct SwitchProps {
    pub id: AttrValue,
    pub checked: bool,
    pub onchange: Callback<bool>,

    #[prop_or_default]
    pub label: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,
}

/// Something that is either on or off, and takes effect as it is flipped.
#[function_component(Switch)]
pub fn switch(props: &SwitchProps) -> Html {
    html! {
        <yewi::ToggleSwitch
            id={props.id.clone()}
            checked={props.checked}
            on_change={props.onchange.clone()}
            disabled={props.disabled}
        >
            { props.label.clone() }
        </yewi::ToggleSwitch>
    }
}

#[derive(Properties, PartialEq)]
pub struct SecretInputProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

/// A value that is masked until somebody asks to see it.
#[function_component(SecretInput)]
pub fn secret_input(props: &SecretInputProps) -> Html {
    html! {
        <yewi::SecretInput
            id={props.id.clone()}
            value={props.value.clone()}
            on_input={props.onchange.clone()}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            invalid={props.invalid}
        />
    }
}
