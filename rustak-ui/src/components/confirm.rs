//! A destructive action that asks first.
//!
//! Revoking a credential strands whatever is using it and forgetting a device
//! loses what we knew about it, and neither is undone by doing it again. The
//! browser's own `confirm()` would do the job, but it cannot say *which* row is
//! about to go and it cannot be driven by a test — so the button replaces itself
//! with the question, in place, and the answer stays next to the thing it is
//! about.

use yew::prelude::*;

use crate::components::{Button, ButtonGroup, ButtonKind};

#[derive(Properties, PartialEq)]
pub struct ConfirmButtonProps {
    /// The label before anybody has asked for anything.
    pub label: AttrValue,

    /// The question, which should name what is about to happen to what.
    pub question: AttrValue,

    /// The label on the button that goes through with it. Says what it does
    /// rather than "Yes", so the two buttons cannot be told apart only by
    /// position.
    #[prop_or(AttrValue::from("Confirm"))]
    pub confirm_label: AttrValue,

    pub onconfirm: Callback<()>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub busy: bool,

    /// Why the action is unavailable, when it is.
    #[prop_or_default]
    pub title: Option<AttrValue>,
}

/// A button that asks before it acts.
#[function_component(ConfirmButton)]
pub fn confirm_button(props: &ConfirmButtonProps) -> Html {
    let asking = use_state(|| false);

    let onask = {
        let asking = asking.clone();
        Callback::from(move |_: MouseEvent| asking.set(true))
    };

    let oncancel = {
        let asking = asking.clone();
        Callback::from(move |_: MouseEvent| asking.set(false))
    };

    let onconfirm = {
        let (asking, onconfirm) = (asking.clone(), props.onconfirm.clone());
        Callback::from(move |_: MouseEvent| {
            asking.set(false);
            onconfirm.emit(());
        })
    };

    if !*asking {
        return html! {
            <Button
                small=true
                kind={ButtonKind::Danger}
                disabled={props.disabled}
                busy={props.busy}
                title={props.title.clone()}
                onclick={onask}
            >
                { props.label.clone() }
            </Button>
        };
    }

    html! {
        <div class="confirm">
            <span class="confirm__question" role="alert">{ props.question.clone() }</span>
            <ButtonGroup label={props.question.clone()}>
                <Button small=true onclick={oncancel}>{ "Cancel" }</Button>
                <Button
                    small=true
                    kind={ButtonKind::Danger}
                    busy={props.busy}
                    onclick={onconfirm}
                >
                    { props.confirm_label.clone() }
                </Button>
            </ButtonGroup>
        </div>
    }
}
