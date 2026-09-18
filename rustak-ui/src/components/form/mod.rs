//! Form controls.
//!
//! Everything somebody types into rustak — a host name, a username, a
//! certificate authority's common name — goes through these, so they are built
//! once here rather than assembled per page. They share one shape: each takes
//! its current `value` and an `onchange` carrying the new one, leaving the state
//! with the caller.
//!
//! # Validation
//!
//! Controls *display* an error; they never decide one. What counts as a valid
//! username or host name is settled by `rustak-api`'s newtypes and by the
//! server, which are the only things that can be authoritative about it — so a
//! control that decided for itself would either duplicate that or contradict it.

// The kit is written as a whole rather than grown one page at a time, so that
// the pages built on it are consistent by construction instead of each inventing
// the control it happens to need first.
#![allow(dead_code)]

mod button;
mod choice;
mod input;

pub use button::{Button, ButtonGroup, ButtonKind};
pub use choice::{Select, SelectOption, Switch};
pub use input::{NumberInput, TextArea, TextInput};

use yew::prelude::*;

/// A labelled row wrapping a single control.
///
/// Owns the label, the required marker, the help text and the error, so that
/// every field in the application lays out identically and a control only has to
/// render its own input.
#[derive(Properties, PartialEq)]
pub struct FieldProps {
    pub label: AttrValue,

    /// Associates the label with the control it describes.
    pub id: AttrValue,

    #[prop_or_default]
    pub required: bool,

    /// Guidance beneath the control. Hidden while an error is shown, because the
    /// error is the more urgent of the two and stacking both pushes the next
    /// field down as somebody types.
    #[prop_or_default]
    pub help: Option<AttrValue>,

    #[prop_or_default]
    pub error: Option<AttrValue>,

    pub children: Children,
}

#[function_component(Field)]
pub fn field(props: &FieldProps) -> Html {
    let mut class = classes!("field");
    if props.error.is_some() {
        class.push("field--invalid");
    }

    html! {
        <div class={class}>
            <label class="field__label" for={props.id.clone()}>
                { &props.label }
                if props.required {
                    <span class="field__required" aria-hidden="true">{ "*" }</span>
                }
            </label>

            <div class="field__control">{ props.children.clone() }</div>

            if let Some(error) = &props.error {
                <p class="field__error" role="alert">{ error }</p>
            } else if let Some(help) = &props.help {
                <p class="field__help">{ help }</p>
            }
        </div>
    }
}
