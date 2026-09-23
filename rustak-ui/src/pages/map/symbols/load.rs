//! A symbol catalogue, fetched the first time a picker wants it.
//!
//! `/vendor/symbology/<edition>.json` is a list of `[code, [name, ...]]`, which
//! `scripts/vendor.mjs` derives from the `mil-std-2525` package at build time.
//! It is about 80 KB an edition and is only wanted by somebody editing a
//! marker, so it is not in the bundle: it is asked for when the editor first
//! opens, kept for the life of the page, and shared by both pickers.
//!
//! A catalogue that cannot be had is not an error anybody is shown. The
//! pickers still take a code that is typed, which is what the form did before
//! it had them; they only stop being able to suggest one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gloo_net::http::Request;
use rustak_api::Symbology;
use yew::prelude::*;

/// A catalogue as it is published: each symbol's short code, and the names
/// that lead to it. See [`super::codes`] for what the code is.
pub type Rows = Rc<Vec<(String, Vec<String>)>>;

thread_local! {
    static HELD: RefCell<HashMap<Symbology, Rows>> = RefCell::new(HashMap::new());
}

async fn fetch(edition: Symbology) -> Option<Rows> {
    let url = format!("/vendor/symbology/{}.json", edition.as_str());
    let response = Request::get(&url).send().await.ok().filter(|r| r.ok())?;

    response.json().await.ok().map(Rc::new)
}

/// The catalogue for `edition`, once it has arrived.
#[hook]
pub fn use_catalogue(edition: Symbology) -> Option<Rows> {
    let held = use_state(|| HELD.with_borrow(|held| held.get(&edition).cloned()));

    {
        let held = held.clone();
        use_effect_with(edition, move |edition| {
            let edition = *edition;
            let known = HELD.with_borrow(|all| all.get(&edition).cloned());

            match known {
                Some(rows) => held.set(Some(rows)),
                None => {
                    held.set(None);
                    wasm_bindgen_futures::spawn_local(async move {
                        if let Some(rows) = fetch(edition).await {
                            HELD.with_borrow_mut(|all| all.insert(edition, rows.clone()));
                            held.set(Some(rows));
                        }
                    });
                }
            }
            || ()
        });
    }

    (*held).clone()
}
