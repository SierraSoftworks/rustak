//! rustak admin UI entry point.
//!
//! M0 only proves the Trunk/Yew build pipeline: routes, the protected-route
//! gate, the login/setup-wizard flows, the dashboard and its fixtures all
//! arrive in a later implementation brief (see
//! `.claude/plan/design/01-foundations-storage-ci.md` §8, step 13).

use yew::prelude::*;

#[function_component(App)]
fn app() -> Html {
    html! {
        <main class="app-shell">
            <h1>{ "rustak" }</h1>
        </main>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<App>::new().render();
}
