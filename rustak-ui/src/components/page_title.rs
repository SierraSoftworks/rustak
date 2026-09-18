use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct PageTitleProps {
    /// The page's primary heading, reflecting the active route.
    pub title: AttrValue,

    /// An optional supporting line describing what the page is for.
    #[prop_or_default]
    pub subtitle: Option<AttrValue>,

    /// Controls aligned to the end of the title row.
    #[prop_or_default]
    pub children: Html,
}

/// The page-specific context shown beneath the app bar.
#[function_component(PageTitle)]
pub fn page_title(props: &PageTitleProps) -> Html {
    let subtitle = match &props.subtitle {
        Some(subtitle) => html! { <p class="page-title__subtitle">{ subtitle.clone() }</p> },
        None => html! {},
    };

    html! {
        <div class="page-title">
            <div class="page-title__text">
                <h1 class="page-title__heading">{ props.title.clone() }</h1>
                { subtitle }
            </div>
            <div class="page-title__actions">{ props.children.clone() }</div>
        </div>
    }
}
