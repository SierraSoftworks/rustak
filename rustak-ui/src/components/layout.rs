use chrono::{Datelike, Utc};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct LayoutProps {
    #[prop_or_default]
    pub children: Html,
}

/// The outer chrome — logo header and footer — shared by the screens that are
/// not inside the admin shell: the landing page, the wizard and the 404.
#[function_component(Layout)]
pub fn layout(props: &LayoutProps) -> Html {
    html! {
        <>
            <div class="header">
                <img
                    src="/logo.svg"
                    alt="The rustak logo."
                />
            </div>

            { props.children.clone() }

            <footer>
                <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
            </footer>
        </>
    }
}
