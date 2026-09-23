//! A picker for one thing out of a large hierarchy: searched by typing, walked
//! by opening branches, and with a picture beside each row when the caller
//! can draw one.
//!
//! The browser's `<select>` is what [`Select`](super::Select) is built on, and
//! its documentation says when to stop using it: when a picker needs to render
//! something the browser cannot. A MIL-STD-2525 catalogue is that case — 863
//! symbols, six levels deep, each of which *is* a picture.
//!
//! # What it keeps from a `<select>`
//!
//! The trigger is a button a `<label for>` names, so a form reads the same to
//! somebody using a screen reader. Open, it is the combobox pattern: the
//! search box keeps the focus, the arrow keys move through the list, Enter
//! chooses, Escape closes and hands the focus back. Right and left open and
//! leave a branch, which is the one thing a flat list has no key for.
//!
//! # What is chosen need not be listed
//!
//! A value the tree does not hold is shown as it stands rather than as
//! nothing: a type somebody's device invented is still that marker's type.
//! With `typed`, the caller lets such a value be entered too — it says whether
//! what was typed is well formed, and this offers it as a last row.

mod model;
mod rows;
mod view;

use std::rc::Rc;

use web_sys::{HtmlElement, HtmlInputElement};
use yew::prelude::*;

pub use model::{Entry, Tree};
use rows::{Row, rows};
use view::{TreeRow, icon};

#[derive(Properties, PartialEq)]
pub struct TreeSelectProps {
    pub id: AttrValue,
    pub tree: Rc<Tree>,

    /// Absent means nothing is chosen, which shows the placeholder.
    pub value: Option<AttrValue>,
    pub onchange: Callback<Option<String>>,

    #[prop_or(AttrValue::Static("Choose one"))]
    pub placeholder: AttrValue,

    /// Lets somebody return to having nothing chosen, under this name.
    #[prop_or_default]
    pub clear_label: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    /// Draws the preview for an item's `icon`.
    #[prop_or_default]
    pub render_icon: Option<Callback<String, Html>>,

    /// Says whether text that names nothing in the tree may be used as it
    /// stands, and as what.
    #[prop_or_default]
    pub typed: Option<Callback<String, Option<String>>>,
}

#[function_component(TreeSelect)]
pub fn tree_select(props: &TreeSelectProps) -> Html {
    let open = use_state(|| false);
    let query = use_state(String::new);
    let branch = use_state(|| None::<usize>);
    let active = use_state(|| 0_usize);
    let (trigger, search) = (use_node_ref(), use_node_ref());

    let tree = &props.tree;
    let chosen = props.value.as_ref().and_then(|value| tree.find(value));
    let typed = props
        .typed
        .as_ref()
        .and_then(|typed| typed.emit(query.trim().to_string()));
    let shown = rows(tree, &query, *branch, props.clear_label.is_some(), typed);
    let list = format!("{}-list", props.id);

    // The search box takes the focus as the panel opens, and the row the keys
    // are on is kept in sight.
    {
        let search = search.clone();
        use_effect_with(*open, move |open| {
            if *open && let Some(input) = search.cast::<HtmlInputElement>() {
                let _ = input.focus();
            }
            || ()
        });
    }
    {
        let at = format!("{}-row-{}", props.id, *active);
        use_effect_with((*open, *active), move |(open, _)| {
            if *open && let Some(row) = gloo_utils::document().get_element_by_id(&at) {
                row.scroll_into_view_with_bool(false);
            }
            || ()
        });
    }

    let close = {
        let (open, query, trigger) = (open.clone(), query.clone(), trigger.clone());
        Callback::from(move |()| {
            open.set(false);
            query.set(String::new());
            if let Some(button) = trigger.cast::<HtmlElement>() {
                let _ = button.focus();
            }
        })
    };
    let toggle = {
        let (open, branch, active, close) =
            (open.clone(), branch.clone(), active.clone(), close.clone());
        // Opens on what is chosen, so the rows around it are the first thing seen.
        let home = chosen
            .and_then(|at| tree.item(at))
            .and_then(|item| item.parent);
        let tree = tree.clone();
        Callback::from(move |_: MouseEvent| match *open {
            true => close.emit(()),
            false => {
                let at = chosen.and_then(|at| tree.level(home).iter().position(|row| *row == at));
                branch.set(home);
                active.set(at.unwrap_or_default());
                open.set(true);
            }
        })
    };
    let enter = {
        let (branch, active, query) = (branch.clone(), active.clone(), query.clone());
        Callback::from(move |to: Option<usize>| {
            branch.set(to);
            active.set(0);
            query.set(String::new());
        })
    };
    let pick = {
        let (onchange, close, enter, tree) = (
            props.onchange.clone(),
            close.clone(),
            enter.clone(),
            tree.clone(),
        );
        Callback::from(move |row: Row| match row {
            Row::Clear => {
                onchange.emit(None);
                close.emit(());
            }
            Row::Typed(value) => {
                onchange.emit(Some(value));
                close.emit(());
            }
            Row::Item(at) => match tree.item(at).and_then(|item| item.value.clone()) {
                Some(value) => {
                    onchange.emit(Some(value));
                    close.emit(());
                }
                // A branch that is not itself a thing: the only use of it is
                // what is inside.
                None => enter.emit(Some(at)),
            },
        })
    };

    let oninput = {
        let (query, active) = (query.clone(), active.clone());
        Callback::from(move |event: InputEvent| {
            if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
                query.set(input.value());
                active.set(0);
            }
        })
    };
    let onkeydown = {
        let (active, close, pick, enter) =
            (active.clone(), close.clone(), pick.clone(), enter.clone());
        let (shown, tree, up) = (
            shown.clone(),
            tree.clone(),
            branch.and_then(|at| tree.item(at)).map(|item| item.parent),
        );
        let walking = query.trim().is_empty();
        Callback::from(move |event: KeyboardEvent| {
            let last = shown.len().saturating_sub(1);
            let inside = match shown.get(*active) {
                Some(Row::Item(at)) if tree.item(*at).is_some_and(|i| !i.children.is_empty()) => {
                    Some(*at)
                }
                _ => None,
            };

            match event.key().as_str() {
                "ArrowDown" => active.set((*active + 1).min(last)),
                "ArrowUp" => active.set(active.saturating_sub(1)),
                "Home" if walking => active.set(0),
                "End" if walking => active.set(last),
                "Enter" => shown
                    .get(*active)
                    .cloned()
                    .into_iter()
                    .for_each(|row| pick.emit(row)),
                "Escape" => close.emit(()),
                // Only while nothing is typed: in a search these move the caret.
                "ArrowRight" if walking && inside.is_some() => enter.emit(inside),
                "ArrowLeft" if walking && up.is_some() => enter.emit(up.flatten()),
                _ => return,
            }
            event.prevent_default();
        })
    };

    // The trail above the list: every step is a way back up to it.
    let trail = branch.map(|at| {
        let mut steps = vec![(None, "All".to_string())];
        let mut next = Some(at);
        let mut above = Vec::new();
        while let Some(step) = next.and_then(|at| tree.item(at).map(|item| (at, item))) {
            above.push((Some(step.0), step.1.label.clone()));
            next = step.1.parent;
        }
        steps.extend(above.into_iter().rev());
        steps
    });

    let current = chosen.and_then(|at| tree.item(at));

    html! {
        <div class={classes!("tree-select", open.then_some("tree-select--open"))}>
            <button
                ref={trigger}
                id={props.id.clone()}
                type="button"
                class="field__input tree-select__trigger"
                aria-haspopup="listbox"
                aria-expanded={open.to_string()}
                disabled={props.disabled}
                onclick={toggle}
            >
                { icon(props.render_icon.as_ref(), current.and_then(|item| item.icon.as_ref())) }
                <span class="tree-select__label">
                    { match (current, &props.value) {
                        (Some(item), _) => item.label.clone(),
                        // Chosen, and not something the tree lists: say what it is.
                        (None, Some(value)) => value.to_string(),
                        (None, None) => props.placeholder.to_string(),
                    } }
                </span>
                if let Some(detail) = current.and_then(|item| item.detail.as_ref()) {
                    <code class="tree-select__detail">{ detail.clone() }</code>
                }
                <span class="tree-select__chevron" aria-hidden="true">{ "▾" }</span>
            </button>

            if *open {
                <div class="tree-select__backdrop" onclick={close.reform(|_: MouseEvent| ())} />
                <div class="tree-select__panel">
                    <input
                        ref={search}
                        class="field__input tree-select__search"
                        type="text"
                        role="combobox"
                        aria-label="Search"
                        aria-expanded="true"
                        aria-controls={list.clone()}
                        aria-autocomplete="list"
                        aria-activedescendant={format!("{}-row-{}", props.id, *active)}
                        autocomplete="off"
                        placeholder="Search by name or code"
                        value={(*query).clone()}
                        {oninput}
                        {onkeydown}
                    />

                    if let Some(trail) = trail.filter(|_| query.trim().is_empty()) {
                        <nav class="tree-select__trail" aria-label="Where this list is">
                            { for trail.into_iter().map(|(to, label)| html! {
                                <button type="button" onclick={enter.reform(move |_: MouseEvent| to)}>
                                    { label }
                                </button>
                            }) }
                        </nav>
                    }

                    <ul id={list} class="tree-select__list" role="listbox">
                        { for shown.iter().enumerate().map(|(n, row)| html! {
                            <TreeRow
                                id={format!("{}-row-{n}", props.id)}
                                row={row.clone()}
                                tree={Rc::clone(tree)}
                                active={n == *active}
                                chosen={match row {
                                    Row::Clear => props.value.is_none(),
                                    Row::Item(at) => chosen == Some(*at),
                                    Row::Typed(_) => false,
                                }}
                                searching={!query.trim().is_empty()}
                                clear_label={props.clear_label.clone()}
                                render_icon={props.render_icon.clone()}
                                onpick={pick.clone()}
                                onenter={enter.clone()}
                            />
                        }) }
                        if shown.is_empty() {
                            <li class="tree-select__none">{ "Nothing by that name." }</li>
                        }
                    </ul>
                </div>
            }
        </div>
    }
}
