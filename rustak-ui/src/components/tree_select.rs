//! One choice from a hierarchy too large for a flat list: a preference out of
//! ATAK's catalogue, a symbol out of MIL-STD-2525.
//!
//! The control is yewi's `TreeSelect`: a trigger that opens a search box over
//! a list that can be walked level by level. What is here is the vocabulary
//! the rest of the console builds its hierarchies in.

use std::rc::Rc;

use yew::prelude::*;
use yewi::{TreeOption, TreeOptions};

/// One entry of a hierarchy, placed by the names that lead to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// The names that lead here, outermost first, ending in this entry's own.
    pub path: Vec<String>,
    /// What choosing it reports. `None` for a branch that only holds others.
    pub value: Option<String>,
    /// Shown small beside the name and searched: a code, for those who know it.
    pub detail: Option<String>,
    /// Handed to the select's `render_icon` to draw a picture before the name.
    pub icon: Option<String>,
}

/// A hierarchy to choose from, built once and shared by reference.
#[derive(Clone, Debug, PartialEq)]
pub struct Tree(TreeOptions);

impl Tree {
    pub fn build(entries: impl IntoIterator<Item = Entry>) -> Self {
        Self(
            entries
                .into_iter()
                .map(|entry| TreeOption {
                    path: entry.path.into_iter().map(AttrValue::from).collect(),
                    value: entry.value.map(AttrValue::from),
                    detail: entry.detail.map(AttrValue::from),
                    icon: None,
                    media: entry.icon.map(AttrValue::from),
                })
                .collect(),
        )
    }
}

#[derive(Properties, PartialEq)]
pub struct TreeSelectProps {
    pub id: AttrValue,
    pub tree: Rc<Tree>,
    pub value: Option<AttrValue>,

    /// Fired with the chosen entry's value, or `None` when the choice is
    /// cleared.
    pub onchange: Callback<Option<String>>,

    #[prop_or(AttrValue::Static("Choose one"))]
    pub placeholder: AttrValue,

    /// Adds a first row, under this name, that returns to nothing chosen.
    #[prop_or_default]
    pub clear_label: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    /// Draws the picture for an entry's `icon`, before its name.
    #[prop_or_default]
    pub render_icon: Option<Callback<String, Html>>,

    /// Lets text that names no entry be used as it stands: called with what
    /// was typed, it returns the value that stands for, or `None` when it is
    /// not well formed.
    #[prop_or_default]
    pub typed: Option<Callback<String, Option<String>>>,
}

#[function_component(TreeSelect)]
pub fn tree_select(props: &TreeSelectProps) -> Html {
    let on_change = props
        .onchange
        .reform(|value: Option<AttrValue>| value.map(|value| value.to_string()));
    let render_media = props
        .render_icon
        .as_ref()
        .map(|render| render.reform(|media: AttrValue| media.to_string()));
    let custom = props.typed.as_ref().map(|typed| {
        let typed = typed.clone();
        Callback::from(move |text: String| typed.emit(text).map(AttrValue::from))
    });

    html! {
        <yewi::TreeSelect
            id={props.id.clone()}
            options={props.tree.0.clone()}
            value={props.value.clone()}
            {on_change}
            placeholder={props.placeholder.clone()}
            clear_label={props.clear_label.clone()}
            disabled={props.disabled}
            {render_media}
            {custom}
            search_label="Search"
        />
    }
}
