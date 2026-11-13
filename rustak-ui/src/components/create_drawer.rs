//! A "make one of these" form, kept out of the page until it is asked for.
//!
//! A list page is about its list. The form that adds to it opens beside the
//! page from a button in the title row, next to the page's other actions, and
//! goes away again once what it made is in the list.

use yew::prelude::*;
use yewi::icons;

use super::chrome::use_page_controls;
use super::{Button, ButtonKind};

#[derive(Properties, PartialEq)]
pub struct CreateDrawerProps {
    /// What the button in the title row says.
    pub button: AttrValue,

    pub title: AttrValue,

    #[prop_or_default]
    pub subtitle: Option<AttrValue>,

    /// The form's own buttons, at the foot of the drawer.
    #[prop_or_default]
    pub actions: Html,

    /// The request is in flight.
    #[prop_or_default]
    pub busy: bool,

    /// The last request was refused, so the form stays for it to be corrected.
    #[prop_or_default]
    pub failed: bool,

    /// Stay open after a request that worked, for a form that says so itself
    /// and may be used again straight away.
    #[prop_or_default]
    pub keep_open: bool,

    #[prop_or_default]
    pub children: Html,
}

#[function_component(CreateDrawer)]
pub fn create_drawer(props: &CreateDrawerProps) -> Html {
    let open = use_state(|| false);

    let onopen = use_callback(open.clone(), |_: MouseEvent, open| open.set(true));
    let on_close = use_callback(open.clone(), |(), open| open.set(false));

    use_page_controls(html! {
        <Button kind={ButtonKind::Primary} icon={icons::PLUS} onclick={onopen}>
            { props.button.clone() }
        </Button>
    });

    // A request that finished without being refused made the thing, and the
    // list behind the drawer is where it now is.
    let was_busy = use_mut_ref(|| false);
    {
        let open = open.clone();
        let settled = (props.busy, props.failed, props.keep_open);
        use_effect_with(settled, move |(busy, failed, keep_open)| {
            let finished = was_busy.replace(*busy) && !*busy;
            if finished && !*failed && !*keep_open {
                open.set(false);
            }
        });
    }

    html! {
        <yewi::Drawer
            open={*open}
            class="create-drawer"
            title={props.title.clone()}
            footer={props.actions.clone()}
            {on_close}
        >
            if let Some(subtitle) = &props.subtitle {
                <p class="create-drawer__lead">{ subtitle.clone() }</p>
            }
            { props.children.clone() }
        </yewi::Drawer>
    }
}
