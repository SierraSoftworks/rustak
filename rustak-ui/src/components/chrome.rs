//! What a page tells the shell around it.
//!
//! The shell owns the title row and the app bar, but what belongs in them is
//! the page's business: its actions, the name of the thing it is showing, and
//! what the search box in the bar is narrowing. A page hands those up through
//! the context here rather than drawing a second header of its own.

use yew::prelude::*;

use crate::app::Route;

/// What a page puts in the title row in place of its route's own heading.
#[derive(Clone, PartialEq, Default)]
pub struct Heading {
    /// The name of the thing the page shows: an account, a mission.
    pub title: AttrValue,
    /// The line under it: an identifier, a kind.
    pub subtitle: Html,
    /// Short status beside the title.
    pub meta: Html,
    /// The list this page is a row of, and the in-app path back to it.
    pub parent: Option<(AttrValue, AttrValue)>,
}

/// The slots of the shell a page can fill.
#[derive(Clone, PartialEq)]
pub struct PageActions {
    pub(super) actions: Callback<Html>,
    pub(super) controls: Callback<Html>,
    pub(super) heading: Callback<Option<Heading>>,
}

impl PageActions {
    /// Puts controls at the end of the title row.
    pub fn set(&self, actions: Html) {
        self.actions.emit(actions);
    }

    pub fn clear(&self) {
        self.actions.emit(Html::default());
    }
}

#[derive(Properties, PartialEq)]
pub struct PageHeadingProps {
    pub title: AttrValue,

    #[prop_or_default]
    pub subtitle: Html,

    #[prop_or_default]
    pub meta: Html,

    /// The label of the list this page belongs to.
    pub parent: AttrValue,

    /// The in-app path of that list.
    pub parent_path: AttrValue,
}

/// Names the page after the thing it shows, with the way back to its list.
/// Renders nothing itself: the shell draws it in the title row.
#[function_component(PageHeading)]
pub fn page_heading(props: &PageHeadingProps) -> Html {
    let chrome = use_context::<PageActions>();
    let heading = Heading {
        title: props.title.clone(),
        subtitle: props.subtitle.clone(),
        meta: props.meta.clone(),
        parent: Some((props.parent.clone(), props.parent_path.clone())),
    };

    use_effect_with((heading, chrome), |(heading, chrome)| {
        if let Some(chrome) = chrome {
            chrome.heading.emit(Some(heading.clone()));
        }
        let chrome = chrome.clone();
        move || {
            if let Some(chrome) = chrome {
                chrome.heading.emit(None);
            }
        }
    });

    Html::default()
}

/// Puts a page's own controls — what narrows or switches its view — in the
/// title row, ahead of its actions, for as long as the page is mounted.
#[hook]
pub fn use_page_controls(controls: Html) {
    let chrome = use_context::<PageActions>();
    use_effect_with((controls, chrome), |(controls, chrome)| {
        if let Some(chrome) = chrome {
            chrome.controls.emit(controls.clone());
        }
        let chrome = chrome.clone();
        move || {
            if let Some(chrome) = chrome {
                chrome.controls.emit(Html::default());
            }
        }
    });
}

/// What the app bar's search box holds, for the page it is narrowing.
#[derive(Clone, PartialEq, Default)]
pub struct PageSearch(pub AttrValue);

/// The text in the app bar's search box, trimmed and lower-cased: what a
/// page compares its rows against. Empty when nothing has been typed.
#[hook]
pub fn use_page_search() -> String {
    use_context::<PageSearch>()
        .map(|search| search.0.trim().to_lowercase())
        .unwrap_or_default()
}

/// What the search box says on a page that has something to search: its
/// accessible name, then its placeholder. Pages with nothing to search show
/// no box.
pub fn search_of(route: &Route) -> Option<(&'static str, &'static str)> {
    match route {
        Route::Euds => Some(("Search devices", "Search callsign, identifier or owner")),
        Route::Missions => Some(("Search missions", "Search name, tool, keyword or channel")),
        Route::Packages => Some(("Search packages", "Search name or keyword")),
        Route::Situation => Some(("Search callsigns", "Search callsign")),
        _ => None,
    }
}
