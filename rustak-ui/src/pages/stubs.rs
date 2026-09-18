//! The pages that exist as destinations before they exist as features.
//!
//! Every route in the navigation is reachable from the first milestone, because
//! a link that goes nowhere is worse than one that says what it is waiting for.
//! Each of these names the milestone that fills it in, so the console is honest
//! about what this build can and cannot do.

use yew::prelude::*;

use crate::components::EmptyState;

#[derive(Properties, PartialEq)]
struct ComingProps {
    /// The milestone that brings this page to life, as `plan.md` numbers them.
    milestone: &'static str,

    /// What it will do, in the present tense, so the reader learns what to
    /// expect rather than only that it is missing.
    summary: &'static str,
}

#[function_component(Coming)]
fn coming(props: &ComingProps) -> Html {
    html! {
        <EmptyState
            title={format!("Arrives in {}", props.milestone)}
            message={props.summary}
        />
    }
}

macro_rules! stub {
    ($name:ident, $render:ident, $milestone:literal, $summary:literal) => {
        #[function_component($name)]
        pub fn $render() -> Html {
            html! { <Coming milestone={$milestone} summary={$summary} /> }
        }
    };
}

stub!(
    Devices,
    devices,
    "M2",
    "Every EUD that has enrolled, what it last connected as, and the certificate \
     it presented — alongside the controls to revoke one."
);

stub!(
    Credentials,
    credentials,
    "M2",
    "One-time enrolment tokens with their QR codes, and the opt-in client \
     passwords that exist only for clients which can do nothing better."
);

stub!(
    Groups,
    groups,
    "M2",
    "The channels this server routes by: who may write into each one, who may \
     read out of it, and which of them a device currently has switched on."
);

stub!(
    Missions,
    missions,
    "M4",
    "Data Sync missions, their subscribers and their contents, with the change \
     log that tells you who added what."
);

stub!(
    Packages,
    packages,
    "M3",
    "The data packages this server hands out, and the files inside them."
);

stub!(
    Profiles,
    profiles,
    "M3",
    "The preferences each device is sent on enrolment and on connection."
);

stub!(
    Services,
    services,
    "M6",
    "The sidecars connected to this server, what they can do, and whether they \
     are still reporting in."
);
