//! The wizard's shape: which steps there are, and where somebody is in them.

use rustak_api::SetupStatus;
use yew::prelude::*;

/// One step of the first-run wizard, in order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// Prove you can read the server's filesystem.
    Token,
    /// Create the first administrator.
    Admin,
    /// Give that administrator a way to sign in.
    Passkey,
    /// Tell the server what it is called.
    Server,
    /// Create the certificate authority everything else hangs off.
    Ca,
    /// Close the wizard for good.
    Done,
}

impl Step {
    pub const ALL: &'static [Self] = &[
        Self::Token,
        Self::Admin,
        Self::Passkey,
        Self::Server,
        Self::Ca,
        Self::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Token => "Setup token",
            Self::Admin => "Administrator",
            Self::Passkey => "Passkey",
            Self::Server => "Server name",
            Self::Ca => "Certificate authority",
            Self::Done => "Finish",
        }
    }

    /// The next step, or [`Step::Done`] at the end.
    pub fn next(self) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|step| *step > self)
            .unwrap_or(Self::Done)
    }

    /// Where a wizard that has already been part-way through should resume.
    ///
    /// The server's own answer is the one that matters: a browser that lost its
    /// tab mid-way has no memory of the steps it finished, and the server does.
    /// The passkey step is skipped on resume because an administrator who exists
    /// but cannot sign in is dealt with by signing in, not by the wizard.
    pub fn resume_from(status: &SetupStatus) -> Self {
        if status.setup_completed {
            Self::Done
        } else if !status.has_admin {
            Self::Token
        } else if !status.has_server_name {
            Self::Server
        } else if !status.has_ca {
            Self::Ca
        } else {
            Self::Done
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct StepperProps {
    pub current: Step,
}

/// The progress indicator across the top of the wizard.
#[function_component(Stepper)]
pub fn stepper(props: &StepperProps) -> Html {
    html! {
        <ol class="stepper">
            { for Step::ALL.iter().enumerate().map(|(index, step)| {
                let state = if *step < props.current {
                    Some("stepper__step--done")
                } else if *step == props.current {
                    Some("stepper__step--current")
                } else {
                    None
                };

                html! {
                    <li class={classes!("stepper__step", state)} key={index}>
                        <span class="stepper__marker">{ index + 1 }</span>
                        <span class="stepper__label">{ step.label() }</span>
                    </li>
                }
            }) }
        </ol>
    }
}
