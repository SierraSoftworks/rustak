//! rustak's admin console.
//!
//! A single-page Yew application served by `rustak-server` from the bundle
//! Trunk writes into `dist/`. It talks to `/api/v1` and to nothing else, so the
//! same bundle works behind any host name without being rebuilt.
//!
//! # Demo mode
//!
//! Appending `?demo` to any URL makes every API call read from
//! [`fixtures`] instead of the network, which is how the interface is developed
//! and reviewed before a server exists to talk to. The substitution happens in
//! the API client, so no page knows demo mode exists — and no page can forget
//! to support it.

mod api;
mod app;
mod auth;
mod components;
mod fixtures;
mod pages;
mod util;

pub use app::Route;

fn main() {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<app::App>::new().render();
}
