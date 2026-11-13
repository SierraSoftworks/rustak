//! One action on its face and the rest behind a chevron.
//!
//! The actions are described as data ([`MenuAction`]) rather than as markup,
//! so that one of them can ask before it happens: a confirmed action opens a
//! dialog that names what is about to be done, and only then runs.

use yew::prelude::*;
use yewi::{Size, Tone};

use super::ButtonKind;
use super::feedback::title_attrs;

/// What an action asks before it runs.
#[derive(Clone, PartialEq)]
pub struct Confirmation {
    pub question: AttrValue,
    pub confirm_label: AttrValue,
}

/// One thing a [`SplitButton`] can do.
#[derive(Clone, PartialEq)]
pub struct MenuAction {
    pub label: AttrValue,
    pub kind: ButtonKind,
    pub disabled: bool,
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

    pub fn danger(mut self) -> Self {
        self.kind = ButtonKind::Danger;
        self
    }

    pub fn primary(mut self) -> Self {
        self.kind = ButtonKind::Primary;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Why it is disabled, or what it does, on hover.
    pub fn title(mut self, title: Option<impl Into<AttrValue>>) -> Self {
        self.title = title.map(Into::into);
        self
    }

    /// Asks `question` first, and runs only once `confirm_label` is pressed.
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

#[derive(Clone, PartialEq)]
pub enum MenuItem {
    Action(MenuAction),
    Separator,
}

#[derive(Properties, PartialEq)]
pub struct SplitButtonProps {
    /// The likeliest action, on the button's face.
    pub primary: MenuAction,

    /// The others, behind the chevron. With none there is only the button.
    #[prop_or_default]
    pub items: Vec<MenuItem>,

    #[prop_or_default]
    pub busy: bool,

    #[prop_or(true)]
    pub small: bool,

    #[prop_or(AttrValue::from("More actions"))]
    pub menu_label: AttrValue,
}

#[function_component(SplitButton)]
pub fn split_button(props: &SplitButtonProps) -> Html {
    let asking = use_state(|| None::<MenuAction>);

    let select = {
        let asking = asking.clone();
        Callback::from(move |action: MenuAction| match action.confirm {
            Some(_) => asking.set(Some(action)),
            None => action.onselect.emit(()),
        })
    };

    let primary = props.primary.clone();
    let on_click = {
        let (select, primary) = (select.clone(), primary.clone());
        Callback::from(move |_: MouseEvent| {
            // The face and the menu are disabled together by yewi, and a face
            // that cannot be used must not take the menu's other actions with
            // it — so the face is left enabled and does nothing instead.
            if !primary.disabled {
                select.emit(primary.clone());
            }
        })
    };

    let item = |item: &MenuItem| match item {
        MenuItem::Separator => html! { <yewi::MenuSeparator /> },
        MenuItem::Action(action) => {
            let on_select = {
                let (select, action) = (select.clone(), action.clone());
                Callback::from(move |_: MouseEvent| select.emit(action.clone()))
            };
            let tone = match action.kind {
                ButtonKind::Danger => Tone::Danger,
                _ => Tone::Neutral,
            };
            html! {
                <yewi::MenuItem
                    {tone}
                    disabled={action.disabled}
                    {on_select}
                    attrs={title_attrs(&action.title)}
                >
                    { action.label.clone() }
                </yewi::MenuItem>
            }
        }
    };

    let size = if props.small {
        Size::Small
    } else {
        Size::Medium
    };
    let only_face = props.items.is_empty();

    let dialog = {
        let confirmation = asking.as_ref().and_then(|action| action.confirm.clone());
        let on_cancel = {
            let asking = asking.clone();
            Callback::from(move |()| asking.set(None))
        };
        let on_confirm = {
            let asking = asking.clone();
            Callback::from(move |()| {
                if let Some(action) = &*asking {
                    action.onselect.emit(());
                }
                asking.set(None);
            })
        };
        let (title, detail) = confirmation
            .as_ref()
            .map(|it| super::confirm::question_parts(&it.question))
            .unwrap_or_default();
        html! {
            <yewi::ConfirmDialog
                open={confirmation.is_some()}
                {title}
                confirm_label={confirmation.map(|it| it.confirm_label).unwrap_or_default()}
                danger=true
                {on_confirm}
                {on_cancel}
            >
                { detail }
            </yewi::ConfirmDialog>
        }
    };

    let mut attrs: Vec<(&'static str, AttrValue)> = Vec::new();
    if let Some(title) = &primary.title {
        attrs.push(("title", title.clone()));
    }

    html! {
        <>
            <yewi::SplitButton
                label={primary.label.clone()}
                variant={primary.kind.variant()}
                {size}
                disabled={primary.disabled && only_face}
                loading={props.busy}
                menu_label={props.menu_label.clone()}
                class={classes!((primary.disabled && !only_face).then_some("split-button--face-off"))}
                {on_click}
                attrs={yewi::Attrs::new(attrs)}
            >
                { for props.items.iter().map(item) }
            </yewi::SplitButton>
            { dialog }
        </>
    }
}
