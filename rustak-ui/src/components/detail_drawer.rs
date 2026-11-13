//! One item of a list, opened beside the list it came from.
//!
//! A row says what a thing is; what can be changed about it opens in a drawer
//! from the row, so the list stays where it was and nothing appears below the
//! fold of a long page.

use yew::prelude::*;
use yewi::Size;

#[derive(Properties, PartialEq)]
pub struct DetailDrawerProps {
    /// Shown for as long as its owner renders it, unless this says otherwise.
    #[prop_or(true)]
    pub open: bool,

    pub title: AttrValue,

    #[prop_or_default]
    pub subtitle: Option<AttrValue>,

    /// For a table, a document or a long form.
    #[prop_or_default]
    pub wide: bool,

    /// The item's own actions, at the foot of the drawer.
    #[prop_or_default]
    pub actions: Html,

    pub on_close: Callback<()>,

    #[prop_or_default]
    pub children: Html,
}

#[function_component(DetailDrawer)]
pub fn detail_drawer(props: &DetailDrawerProps) -> Html {
    html! {
        <yewi::Drawer
            open={props.open}
            class="detail-drawer"
            size={if props.wide { Size::Large } else { Size::Medium }}
            title={props.title.clone()}
            footer={props.actions.clone()}
            on_close={props.on_close.clone()}
        >
            if let Some(subtitle) = &props.subtitle {
                <p class="create-drawer__lead">{ subtitle.clone() }</p>
            }
            { props.children.clone() }
        </yewi::Drawer>
    }
}
