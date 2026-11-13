//! Asking before something that cannot be taken back.

use yew::prelude::*;

use super::{Button, ButtonKind};

/// A question as a dialog shows it: what is asked as the title, and whatever
/// was said after it as the body beneath.
pub fn question_parts(question: &str) -> (AttrValue, Html) {
    match question.split_once('?') {
        Some((asked, rest)) if !rest.trim().is_empty() => (
            AttrValue::from(format!("{asked}?")),
            html! { <p>{ rest.trim().to_string() }</p> },
        ),
        _ => (AttrValue::from(question.to_string()), Html::default()),
    }
}

#[derive(Properties, PartialEq)]
pub struct ConfirmButtonProps {
    pub label: AttrValue,

    /// What is asked before it happens: "Revoke this credential?"
    pub question: AttrValue,

    #[prop_or(AttrValue::from("Confirm"))]
    pub confirm_label: AttrValue,

    pub onconfirm: Callback<()>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub busy: bool,

    /// Row-sized by default; off beside full-sized buttons.
    #[prop_or(true)]
    pub small: bool,

    #[prop_or_default]
    pub title: Option<AttrValue>,
}

/// A destructive action that asks first, in a dialog that names what is about
/// to happen and keeps the way back as easy as the way forward.
#[function_component(ConfirmButton)]
pub fn confirm_button(props: &ConfirmButtonProps) -> Html {
    let asking = use_state(|| false);

    let onask = {
        let asking = asking.clone();
        Callback::from(move |_: MouseEvent| asking.set(true))
    };
    let on_cancel = {
        let asking = asking.clone();
        Callback::from(move |()| asking.set(false))
    };
    let on_confirm = {
        let (asking, onconfirm) = (asking.clone(), props.onconfirm.clone());
        Callback::from(move |()| {
            asking.set(false);
            onconfirm.emit(());
        })
    };

    let (title, detail) = question_parts(&props.question);

    html! {
        <>
            <Button
                small={props.small}
                kind={ButtonKind::Danger}
                disabled={props.disabled}
                busy={props.busy}
                title={props.title.clone()}
                onclick={onask}
            >
                { props.label.clone() }
            </Button>
            <yewi::ConfirmDialog
                open={*asking}
                title={title}
                confirm_label={props.confirm_label.clone()}
                danger=true
                {on_confirm}
                {on_cancel}
            >
                { detail }
            </yewi::ConfirmDialog>
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_the_title_and_what_follows_is_the_body() {
        let (title, detail) = question_parts("Delete 'x'? Its files go with it.");
        assert_eq!(title, "Delete 'x'?");
        assert_ne!(detail, Html::default());

        let (title, detail) = question_parts("Delete 'x'?");
        assert_eq!(title, "Delete 'x'?");
        assert_eq!(detail, Html::default());
    }
}
