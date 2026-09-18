//! The first-run wizard.
//!
//! A freshly installed server has no administrator, no certificate authority and
//! no idea what host name it will be reached on. This fills those in from the
//! browser so that bringing one up does not mean hand-writing TOML — and then
//! closes itself for good, because a wizard that stayed open would be a way to
//! take over a running installation.
//!
//! Which step it opens on comes from `GET /api/v1/setup/status` rather than from
//! anything this tab remembers, so a wizard resumed in a new browser picks up
//! where the *server* got to.

mod admin;
mod server;
mod steps;

use rustak_api::Username;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{Alert, AlertKind, Layout, LoadingNote};
#[cfg(debug_assertions)]
use crate::fixtures;

use admin::{AdminStep, PasskeyStep, TokenStep};
use server::{CaStep, DoneStep, ServerStep};
use steps::{Step, Stepper};

#[function_component(Setup)]
pub fn setup() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let step = use_state(|| None::<Step>);
    let completed = use_state(|| false);
    let error = use_state(|| None::<String>);

    let token = use_state(String::new);
    let admin = use_state(|| None::<(Username, String)>);
    let generation = use_state(|| 0u32);

    // Ask the server where it got to. Once, in every build that matters — the
    // generation only ever moves in a demo build, where the wizard can be put
    // back to the start.
    {
        let (step, completed, error) = (step.clone(), completed.clone(), error.clone());
        use_effect_with(*generation, move |_| {
            spawn_local(async move {
                match api::setup::status().await {
                    Ok(status) => {
                        completed.set(status.setup_completed);
                        step.set(Some(Step::resume_from(&status)));
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
            || ()
        });
    }

    // Puts the wizard back to the start. Demo builds only; see [`DemoReset`].
    let on_reset = {
        let (step, completed, token, admin, generation) = (
            step.clone(),
            completed.clone(),
            token.clone(),
            admin.clone(),
            generation.clone(),
        );
        Callback::from(move |_| {
            step.set(None);
            completed.set(false);
            token.set(String::new());
            admin.set(None);
            generation.set(*generation + 1);
        })
    };

    let advance = {
        let step = step.clone();
        Callback::from(move |_| {
            if let Some(current) = *step {
                step.set(Some(current.next()));
            }
        })
    };

    let on_back = {
        let step = step.clone();
        Callback::from(move |_| step.set(Some(Step::Token)))
    };

    let on_created = {
        let (admin, step) = (admin.clone(), step.clone());
        Callback::from(move |created: (Username, String)| {
            admin.set(Some(created));
            step.set(Some(Step::Passkey));
        })
    };

    // The wizard's later steps are administrator-only, so the session the
    // passkey step established has to be picked up by the rest of the app.
    let on_registered = {
        let (step, refresh) = (step.clone(), auth.refresh.clone());
        Callback::from(move |_| {
            refresh.emit(());
            step.set(Some(Step::Server));
        })
    };

    let on_complete = {
        let (step, completed, refresh) = (step.clone(), completed.clone(), auth.refresh.clone());
        Callback::from(move |_| {
            completed.set(true);
            refresh.emit(());
            step.set(Some(Step::Done));
        })
    };

    let body = match (&*step, &*error) {
        (_, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not reach this server"
                message={message.clone()}
            />
        },
        (None, None) => html! { <LoadingNote label="Checking what this server needs…" /> },
        (Some(current), None) => html! {
            <>
                <Stepper current={*current} />
                <div class="wizard__step">
                    { render_step(*current, &token, &admin, &advance, &on_created,
                        &on_back, &on_registered, &on_complete, *completed) }
                </div>
            </>
        },
    };

    html! {
        <Layout>
            <main class="wizard">
                <div class="wizard__inner">
                    <h1 class="wizard__title">{ "Set up rustak" }</h1>
                    { body }
                    <DemoReset on_reset={on_reset} />
                </div>
            </main>
        </Layout>
    }
}

#[allow(clippy::too_many_arguments)]
fn render_step(
    current: Step,
    token: &UseStateHandle<String>,
    admin: &UseStateHandle<Option<(Username, String)>>,
    advance: &Callback<()>,
    on_created: &Callback<(Username, String)>,
    on_back: &Callback<()>,
    on_registered: &Callback<()>,
    on_complete: &Callback<()>,
    completed: bool,
) -> Html {
    match current {
        Step::Token => {
            let token_state = token.clone();
            html! {
                <TokenStep
                    token={(**token).clone()}
                    on_token={Callback::from(move |value| token_state.set(value))}
                    on_next={advance.clone()}
                />
            }
        }
        Step::Admin => html! {
            <AdminStep
                token={(**token).clone()}
                on_created={on_created.clone()}
                on_back={on_back.clone()}
            />
        },
        Step::Passkey => match &**admin {
            Some((username, registration_token)) => html! {
                <PasskeyStep
                    username={username.clone()}
                    registration_token={registration_token.clone()}
                    on_registered={on_registered.clone()}
                />
            },
            // Only reachable by reloading mid-wizard: the registration token
            // lived in this tab, and it is short-lived by design.
            None => html! {
                <Alert
                    kind={AlertKind::Warning}
                    title="The registration token has been lost"
                    message="It only lives in the browser tab that created the \
                        administrator. Sign in with a passkey if you already registered \
                        one, or register one with the rustak command line."
                />
            },
        },
        Step::Server => html! { <ServerStep on_saved={advance.clone()} /> },
        Step::Ca => html! { <CaStep on_created={advance.clone()} /> },
        Step::Done => html! {
            <DoneStep completed={completed} on_complete={on_complete.clone()} />
        },
    }
}

#[derive(Properties, PartialEq)]
pub struct DemoResetProps {
    pub on_reset: Callback<()>,
}

/// A way back to the start of the wizard, for reviewing it without a server.
///
/// Debug builds in demo mode only — there is no such thing as un-completing
/// setup on a real installation, and offering a button that looked like it would
/// is worse than not having one.
///
/// It resets the store and asks the page to re-read it rather than reloading the
/// tab, because the demo store lives for exactly as long as the page does: a
/// reload would put back the installation it had just cleared.
#[function_component(DemoReset)]
fn demo_reset(#[allow(unused_variables)] props: &DemoResetProps) -> Html {
    #[cfg(debug_assertions)]
    if fixtures::is_demo() {
        let on_reset = props.on_reset.clone();
        let onclick = Callback::from(move |_: MouseEvent| {
            fixtures::reset_setup();
            on_reset.emit(());
        });

        return html! {
            <p class="wizard__demo">
                { "Demo mode. " }
                <button type="button" class="link-button" {onclick}>
                    { "Start the wizard again" }
                </button>
            </p>
        };
    }

    html! {}
}
