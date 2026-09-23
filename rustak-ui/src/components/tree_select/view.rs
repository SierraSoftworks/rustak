//! One row of a [`TreeSelect`](super::TreeSelect)'s list.

use std::rc::Rc;

use yew::prelude::*;

use super::model::Tree;
use super::rows::Row;

/// The preview for an item, the box one would be in for an item without one,
/// and nothing at all for a picker that draws none.
pub fn icon(render: Option<&Callback<String, Html>>, icon: Option<&String>) -> Html {
    match (render, icon) {
        (Some(render), Some(icon)) => render.emit(icon.clone()),
        (Some(_), None) => html! { <span class="tree-select__icon" /> },
        (None, _) => Html::default(),
    }
}

#[derive(Properties, PartialEq)]
pub struct TreeRowProps {
    pub id: AttrValue,
    pub row: Row,
    pub tree: Rc<Tree>,

    /// Whether the keys are on this row.
    pub active: bool,
    /// Whether this row is what the field currently holds.
    pub chosen: bool,
    /// Whether the list is a search, which is when a row says where it lives.
    pub searching: bool,

    pub clear_label: Option<AttrValue>,
    pub render_icon: Option<Callback<String, Html>>,

    pub onpick: Callback<Row>,
    /// Opens the branch this row is, without choosing it.
    pub onenter: Callback<Option<usize>>,
}

#[function_component(TreeRow)]
pub fn tree_row(props: &TreeRowProps) -> Html {
    let class = classes!(
        "tree-select__row",
        props.active.then_some("tree-select__row--active"),
    );
    let onclick = props.onpick.reform({
        let row = props.row.clone();
        move |_: MouseEvent| row.clone()
    });
    let render = props.render_icon.as_ref();
    let selected = props.chosen.to_string();

    match &props.row {
        Row::Clear => html! {
            <li id={props.id.clone()} {class} role="option" aria-selected={selected} {onclick}>
                { icon(render, None) }
                <span class="tree-select__label">{ props.clear_label.clone().unwrap_or_default() }</span>
            </li>
        },
        Row::Typed(value) => html! {
            <li id={props.id.clone()} {class} role="option" aria-selected={selected} {onclick}>
                { icon(render, None) }
                <span class="tree-select__label">
                    { "Use " }<code>{ value.clone() }</code>{ " as typed" }
                </span>
            </li>
        },
        Row::Item(at) => {
            let Some(item) = props.tree.item(*at) else {
                return Html::default();
            };
            let inside = (!item.children.is_empty()).then(|| {
                let (onenter, at) = (props.onenter.clone(), *at);
                Callback::from(move |event: MouseEvent| {
                    event.stop_propagation();
                    onenter.emit(Some(at));
                })
            });
            let above = props.tree.trail(*at).join(" › ");

            html! {
                <li id={props.id.clone()} {class} role="option" aria-selected={selected} {onclick}>
                    { icon(render, item.icon.as_ref()) }
                    <span class="tree-select__label">
                        { item.label.clone() }
                        if props.searching && !above.is_empty() {
                            <small>{ above }</small>
                        }
                    </span>
                    if let Some(detail) = &item.detail {
                        <code class="tree-select__detail">{ detail.clone() }</code>
                    }
                    if let Some(onclick) = inside {
                        <button
                            type="button"
                            class="tree-select__inside"
                            tabindex="-1"
                            aria-label={format!("Show what is inside {}", item.label)}
                            {onclick}
                        >
                            { "›" }
                        </button>
                    }
                </li>
            }
        }
    }
}
