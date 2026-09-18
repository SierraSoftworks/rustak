//! Loading something from the API, and saying so while it happens.
//!
//! Every list page does the same four things: fetch on mount, show a line while
//! it waits, show an alert when it fails, and offer a way to try again. Written
//! once here, they are the same four things on every page — including the
//! refresh button, which lands in the shell's shared title row rather than
//! somewhere each page chose for itself.

use std::future::Future;

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::ApiError;
use crate::components::{PageActions, RefreshButton};

/// The state of one fetched resource.
pub struct Loaded<T> {
    /// What was fetched, once something has been. It survives a failed reload,
    /// so a page that could not refresh keeps showing what it last had rather
    /// than emptying itself.
    pub data: Option<T>,

    pub error: Option<String>,

    pub busy: bool,

    /// Fetches it again.
    pub reload: Callback<()>,
}

/// Fetches a resource on mount, and again whenever [`Loaded::reload`] is called.
#[hook]
pub fn use_resource<T, F, Fut>(fetch: F) -> Loaded<T>
where
    T: Clone + 'static,
    F: FnOnce() -> Fut + 'static,
    Fut: Future<Output = Result<T, ApiError>> + 'static,
{
    let data = use_state(|| None::<T>);
    let error = use_state(|| None::<String>);
    let busy = use_state(|| true);
    let generation = use_state(|| 0u32);

    {
        let (data, error, busy) = (data.clone(), error.clone(), busy.clone());
        use_effect_with(*generation, move |_| {
            busy.set(true);
            spawn_local(async move {
                match fetch().await {
                    Ok(value) => {
                        data.set(Some(value));
                        error.set(None);
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
            || ()
        });
    }

    let reload = {
        let generation = generation.clone();
        Callback::from(move |_| generation.set(*generation + 1))
    };

    Loaded {
        data: (*data).clone(),
        error: (*error).clone(),
        busy: *busy,
        reload,
    }
}

/// Puts a refresh button in the shell's title row for as long as this page is
/// mounted, and takes it away again when the page goes.
#[hook]
pub fn use_refresh_action(reload: Callback<()>, busy: bool) {
    let actions = use_context::<PageActions>();

    use_effect_with((busy, actions), move |(busy, actions)| {
        let busy = *busy;
        if let Some(actions) = actions {
            let onclick = Callback::from(move |_: MouseEvent| reload.emit(()));
            actions.set(html! { <RefreshButton {onclick} {busy} /> });
        }

        let actions = actions.clone();
        move || {
            if let Some(actions) = actions {
                actions.clear();
            }
        }
    });
}
