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
use crate::api::download::{Download, save};
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

/// A file being fetched so the browser can save it.
pub struct Downloading {
    /// True while the request is in flight, so the button cannot be pressed
    /// twice and the reader knows something is happening — a mission archive
    /// takes as long as it takes.
    pub busy: bool,

    /// Why it did not arrive. Shown rather than swallowed, because the useful
    /// refusals here say something worth acting on: "that profile has no
    /// preferences and no files, so a device would receive nothing".
    pub error: Option<String>,

    pub start: Callback<()>,
}

/// Fetches a file on demand and hands it to the browser to save.
///
/// Two steps rather than one, and deliberately: a browser that has already
/// been told to download something cannot then be told the request failed, so
/// the bytes are fetched first and only saved once there is something to save.
#[hook]
pub fn use_download<F, Fut>(fetch: F) -> Downloading
where
    F: Fn() -> Fut + 'static,
    Fut: Future<Output = Result<Download, ApiError>> + 'static,
{
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let start = {
        let (busy, error) = (busy.clone(), error.clone());
        Callback::from(move |_| {
            if *busy {
                return;
            }

            let (busy, error) = (busy.clone(), error.clone());
            let request = fetch();

            busy.set(true);
            spawn_local(async move {
                match request.await {
                    Ok(file) => error.set(save(&file).err().map(|err| err.to_string())),
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    Downloading {
        busy: *busy,
        error: (*error).clone(),
        start,
    }
}
