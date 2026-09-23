//! A symbol catalogue, fetched the first time a picker wants it.
//!
//! `/vendor/symbology/<edition>.json` is a list of `[code, [name, ...]]`, which
//! `scripts/vendor.mjs` derives from the `mil-std-2525` package at build time.
//! It is about 80 KB an edition and is only wanted by somebody editing a
//! marker, so it is not in the bundle: it is asked for when the editor first
//! opens, kept for the life of the page, and shared by both pickers.
//!
//! # Asked for once, and only believed while it is still wanted
//!
//! The two pickers often want the same edition at the same moment, so a fetch
//! that is already under way is joined rather than repeated. And an editor can
//! change its mind — a 2525D code typed into a 2525C list changes the edition
//! — so whoever asked is only told if it is still asking: an answer for an
//! edition that is no longer wanted never reaches the picker that moved on.
//!
//! A catalogue that cannot be had is not an error anybody is shown. The
//! pickers still take a code that is typed, which is what the form did before
//! it had them; they only stop being able to suggest one. Whoever was waiting
//! is let go, and the next editor to open asks again.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gloo_net::http::Request;
use rustak_api::Symbology;
use yew::prelude::*;

/// A catalogue as it is published: each symbol's short code, and the names
/// that lead to it. See [`super::codes`] for what the code is.
pub type Rows = Rc<Vec<(String, Vec<String>)>>;

/// Somebody to tell when a catalogue arrives.
type Waiter = Box<dyn FnOnce(Rows)>;

thread_local! {
    static HELD: RefCell<HashMap<Symbology, Rows>> = RefCell::new(HashMap::new());
    static WAITING: RefCell<HashMap<Symbology, Vec<Waiter>>> = RefCell::new(HashMap::new());
}

async fn fetch(edition: Symbology) -> Option<Rows> {
    let url = format!("/vendor/symbology/{}.json", edition.as_str());
    let response = Request::get(&url).send().await.ok().filter(|r| r.ok())?;

    response.json().await.ok().map(Rc::new)
}

/// Tells `waiter` when `edition` has arrived, starting the fetch only if
/// nobody has already.
fn want(edition: Symbology, waiter: Waiter) {
    let first = WAITING.with_borrow_mut(|waiting| {
        let queue = waiting.entry(edition).or_default();
        queue.push(waiter);
        queue.len() == 1
    });
    if !first {
        return;
    }

    wasm_bindgen_futures::spawn_local(async move {
        let fetched = fetch(edition).await;
        let waiters = WAITING
            .with_borrow_mut(|waiting| waiting.remove(&edition))
            .unwrap_or_default();

        if let Some(rows) = fetched {
            HELD.with_borrow_mut(|held| held.insert(edition, rows.clone()));
            waiters.into_iter().for_each(|tell| tell(rows.clone()));
        }
    });
}

/// The catalogue for `edition`, once it has arrived.
#[hook]
pub fn use_catalogue(edition: Symbology) -> Option<Rows> {
    let held = use_state(|| HELD.with_borrow(|held| held.get(&edition).cloned()));

    {
        let held = held.clone();
        use_effect_with(edition, move |edition| {
            // Cleared when this edition stops being the one wanted, which is
            // what keeps a slow answer for it away from its successor.
            let wanted = Rc::new(Cell::new(true));
            let known = HELD.with_borrow(|all| all.get(edition).cloned());

            if known.is_none() {
                let (held, wanted) = (held.clone(), wanted.clone());
                want(
                    *edition,
                    Box::new(move |rows| {
                        if wanted.get() {
                            held.set(Some(rows));
                        }
                    }),
                );
            }
            held.set(known);

            move || wanted.set(false)
        });
    }

    (*held).clone()
}
