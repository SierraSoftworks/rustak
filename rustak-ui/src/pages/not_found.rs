use yew::prelude::*;
use yew_router::prelude::*;

use crate::Route;
use crate::components::{Center, Layout};

/// Shown for a route nothing matched.
#[function_component(NotFound)]
pub fn not_found() -> Html {
    html! {
        <Layout>
            <Center>
                <div class="auth-card">
                    <h1 class="auth-card__title">{ "Page not found" }</h1>
                    <p class="auth-card__lead">
                        { "There is nothing at this address." }
                    </p>
                    <Link<Route> to={Route::Landing} classes="btn btn--primary btn--lg">
                        { "Back to the start" }
                    </Link<Route>>
                </div>
            </Center>
        </Layout>
    }
}
