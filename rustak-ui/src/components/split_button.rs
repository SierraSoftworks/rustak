//! A split button: one action on its face, the rest behind a caret.
//!
//! A row with three ways to revoke a certificate and a way to forget the
//! device cannot show four buttons and still say what the row is about. So it
//! shows the one an administrator usually wants and puts the rest in a menu
//! that opens on the row, closes when anything else is clicked or `Escape` is
//! pressed, and asks first when an item is destructive — in place, the same
//! way [`crate::components::ConfirmButton`] does.

use wasm_bindgen::JsCast;
use yew::prelude::*;

use crate::components::{Button, ButtonGroup, ButtonKind};

/// The question a destructive action asks before it goes ahead.
#[derive(Clone, PartialEq)]
pub struct Confirmation {
    /// Names what is about to happen to what.
    pub question: AttrValue,

    /// The label on the button that goes through with it. Says what it does
    /// rather than "Yes", so the two cannot be told apart only by position.
    pub confirm_label: AttrValue,
}

/// An action a [`SplitButton`] offers, on its face or in its menu.
#[derive(Clone, PartialEq)]
pub struct MenuAction {
    pub label: AttrValue,
    pub kind: ButtonKind,
    pub disabled: bool,
    /// Why it is unavailable, when it is, or what it does when the label alone
    /// does not say.
    pub title: Option<AttrValue>,
    pub confirm: Option<Confirmation>,
    pub onselect: Callback<()>,
}

impl MenuAction {
    pub fn new(label: impl Into<AttrValue>, onselect: Callback<()>) -> Self {
        Self {
            label: label.into(),
            kind: ButtonKind::Default,
            disabled: false,
            title: None,
            confirm: None,
            onselect,
        }
    }

    /// Marks it as one that destroys something.
    pub fn danger(mut self) -> Self {
        self.kind = ButtonKind::Danger;
        self
    }

    /// Marks it as the one action its card exists for.
    pub fn primary(mut self) -> Self {
        self.kind = ButtonKind::Primary;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn title(mut self, title: Option<impl Into<AttrValue>>) -> Self {
        self.title = title.map(Into::into);
        self
    }

    /// Asks first, with a question that names what is about to go.
    pub fn confirm(
        mut self,
        question: impl Into<AttrValue>,
        confirm_label: impl Into<AttrValue>,
    ) -> Self {
        self.confirm = Some(Confirmation {
            question: question.into(),
            confirm_label: confirm_label.into(),
        });
        self
    }
}

/// One entry in the menu.
#[derive(Clone, PartialEq)]
pub enum MenuItem {
    Action(MenuAction),
    /// A rule between two groups of actions that do different kinds of thing.
    Separator,
}

impl From<MenuAction> for MenuItem {
    fn from(action: MenuAction) -> Self {
        Self::Action(action)
    }
}

#[derive(Properties, PartialEq)]
pub struct SplitButtonProps {
    /// The action shown on the button itself.
    pub primary: MenuAction,

    /// The rest. With none, this is a plain button with no caret.
    #[prop_or_default]
    pub items: Vec<MenuItem>,

    /// A request is in flight: every action is disabled and the face says so.
    #[prop_or_default]
    pub busy: bool,

    #[prop_or(true)]
    pub small: bool,

    /// What the menu holds, for anything reading the caret aloud — "More
    /// actions for QUINN" rather than a bare caret.
    #[prop_or(AttrValue::from("More actions"))]
    pub menu_label: AttrValue,
}

#[function_component(SplitButton)]
pub fn split_button(props: &SplitButtonProps) -> Html {
    let open = use_state(|| false);
    // The question a destructive action is asking, and what to do on "yes".
    let asking = use_state(|| None::<(Confirmation, Callback<()>)>);
    let menu_ref = use_node_ref();

    // A menu that opens under the pointer is no use to a keyboard: put the
    // focus on its first item, so arrowing and tabbing start inside it.
    {
        let menu_ref = menu_ref.clone();
        use_effect_with(*open, move |open| {
            if *open
                && let Some(menu) = menu_ref.cast::<web_sys::Element>()
                && let Ok(Some(first)) = menu.query_selector(".menu__item:not(:disabled)")
                && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
            {
                let _ = first.focus();
            }
            || ()
        });
    }

    let select = {
        let (open, asking) = (open.clone(), asking.clone());
        Callback::from(move |action: MenuAction| {
            open.set(false);
            match action.confirm {
                Some(confirmation) => asking.set(Some((confirmation, action.onselect))),
                None => action.onselect.emit(()),
            }
        })
    };

    if let Some((confirmation, onselect)) = &*asking {
        let oncancel = {
            let asking = asking.clone();
            Callback::from(move |_: MouseEvent| asking.set(None))
        };
        let onconfirm = {
            let (asking, onselect) = (asking.clone(), onselect.clone());
            Callback::from(move |_: MouseEvent| {
                asking.set(None);
                onselect.emit(());
            })
        };

        return html! {
            <div class="confirm">
                <span class="confirm__question" role="alert">{ confirmation.question.clone() }</span>
                <ButtonGroup label={confirmation.question.clone()}>
                    <Button small={props.small} onclick={oncancel}>{ "Cancel" }</Button>
                    <Button
                        small={props.small}
                        kind={ButtonKind::Danger}
                        busy={props.busy}
                        onclick={onconfirm}
                    >
                        { confirmation.confirm_label.clone() }
                    </Button>
                </ButtonGroup>
            </div>
        };
    }

    let toggle = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };
    let close = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(false))
    };
    let onkeydown = {
        let open = open.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                open.set(false);
            }
        })
    };

    let primary = props.primary.clone();
    let on_primary = {
        let (select, primary) = (select.clone(), primary.clone());
        Callback::from(move |_: MouseEvent| select.emit(primary.clone()))
    };

    let item = |item: &MenuItem| match item {
        MenuItem::Separator => html! { <div class="menu__separator" role="separator" /> },
        MenuItem::Action(action) => {
            let onclick = {
                let (select, action) = (select.clone(), action.clone());
                Callback::from(move |_: MouseEvent| select.emit(action.clone()))
            };
            html! {
                <button
                    type="button"
                    role="menuitem"
                    class={classes!(
                        "menu__item",
                        (action.kind == ButtonKind::Danger).then_some("menu__item--danger"),
                    )}
                    disabled={action.disabled}
                    title={action.title.clone()}
                    {onclick}
                >
                    { action.label.clone() }
                </button>
            }
        }
    };

    html! {
        <div class={classes!("split-btn", open.then_some("split-btn--open"))} {onkeydown}>
            <Button
                small={props.small}
                kind={primary.kind}
                disabled={primary.disabled}
                busy={props.busy}
                title={primary.title.clone()}
                onclick={on_primary}
            >
                { primary.label.clone() }
            </Button>

            if !props.items.is_empty() {
                <button
                    type="button"
                    class={classes!(
                        "btn",
                        "split-btn__toggle",
                        props.small.then_some("btn--small"),
                        match primary.kind {
                            ButtonKind::Danger => Some("btn--danger"),
                            ButtonKind::Primary => Some("btn--primary"),
                            _ => None,
                        },
                    )}
                    aria-haspopup="menu"
                    aria-expanded={if *open { "true" } else { "false" }}
                    aria-label={props.menu_label.clone()}
                    title={props.menu_label.clone()}
                    disabled={props.busy}
                    onclick={toggle}
                >
                    <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor"
                        stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"
                        aria-hidden="true">
                        <polyline points="6 9 12 15 18 9" />
                    </svg>
                </button>

                if *open {
                    <div class="split-btn__backdrop" aria-hidden="true" onclick={close} />
                    <div class="menu" role="menu" aria-label={props.menu_label.clone()} ref={menu_ref}>
                        { for props.items.iter().map(item) }
                    </div>
                }
            }
        </div>
    }
}
