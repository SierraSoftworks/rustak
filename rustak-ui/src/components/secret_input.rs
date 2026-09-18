//! A secret, masked until whoever is holding it asks to see it.
//!
//! Masking a setup token is worth doing — these get typed in over somebody's
//! shoulder, and a screen share is the usual way one leaks. Masking it
//! *permanently* is not: a setup token has to be compared against what the
//! server logged, and nobody can do that without reading it back. So the mask is
//! a default, not a wall.
//!
//! Nothing here is ever logged, and the value never leaves the field except
//! through the caller's `onchange`.

use web_sys::HtmlInputElement;
use yew::prelude::*;

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

#[function_component(SecretInput)]
pub fn secret_input(props: &SecretInputProps) -> Html {
    let revealed = use_state(|| false);

    let oninput = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
                onchange.emit(input.value());
            }
        })
    };

    let on_reveal = {
        let revealed = revealed.clone();
        Callback::from(move |_: MouseEvent| revealed.set(!*revealed))
    };

    // Nothing to unmask while the field is empty, so the button that would do it
    // is not drawn — but the room it takes is reserved either way, so the
    // placeholder does not reflow the moment somebody types.
    let unmaskable = !props.value.is_empty();

    html! {
        <div class="secret-input">
            <input
                id={props.id.clone()}
                class={classes!(
                    "field__input",
                    "secret-input__input",
                    props.invalid.then_some("field__input--invalid"),
                )}
                type={if *revealed { "text" } else { "password" }}
                value={props.value.clone()}
                placeholder={props.placeholder.clone()}
                disabled={props.disabled}
                autocomplete="off"
                spellcheck="false"
                {oninput}
            />

            <div class="secret-input__actions">
                if unmaskable {
                    <button
                        type="button"
                        class="secret-input__action"
                        onclick={on_reveal}
                        disabled={props.disabled}
                        title={if *revealed { "Hide" } else { "Show" }}
                        aria-label={if *revealed { "Hide the secret" } else { "Show the secret" }}
                        aria-pressed={revealed.to_string()}
                    >
                        if *revealed { { eye_off_icon() } } else { { eye_icon() } }
                    </button>
                }
            </div>
        </div>
    }
}

fn eye_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
            <circle cx="12" cy="12" r="3" />
        </svg>
    }
}

fn eye_off_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
            <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
            <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
            <line x1="1" y1="1" x2="23" y2="23" />
        </svg>
    }
}
