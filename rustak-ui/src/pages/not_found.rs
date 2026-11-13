use crate::components::{Button, ButtonKind, Center, Layout};
use yew::prelude::*;

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
                    <Button kind={ButtonKind::Primary} large=true href="/">
                        { "Back to the start" }
                    </Button>
                </div>
            </Center>
        </Layout>
    }
}
