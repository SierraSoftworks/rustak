//! The views of one thing, a tab each.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct TabBarProps {
    /// Each tab's label, which is also what identifies it.
    pub tabs: Vec<&'static str>,

    pub current: &'static str,

    /// Fired with the label of the tab that was chosen.
    pub onselect: Callback<&'static str>,

    /// The chosen tab's view. One body for every tab, because which view is
    /// drawn is the page's decision, made from the same state.
    #[prop_or_default]
    pub children: Html,
}

#[function_component(TabBar)]
pub fn tab_bar(props: &TabBarProps) -> Html {
    let on_change = {
        let (tabs, onselect) = (props.tabs.clone(), props.onselect.clone());
        Callback::from(move |value: AttrValue| {
            if let Some(tab) = tabs.iter().find(|tab| **tab == value.as_str()) {
                onselect.emit(tab);
            }
        })
    };

    html! {
        <yewi::Tabs
            class="tab-bar"
            selected={AttrValue::from(props.current)}
            {on_change}
            panel={props.children.clone()}
        >
            { for props.tabs.iter().map(|tab| html_nested! {
                <yewi::Tab value={*tab} label={*tab} />
            }) }
        </yewi::Tabs>
    }
}
