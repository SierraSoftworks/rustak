//! Values that are handed over once: a labelled value with its copy button,
//! and the panel that shows a secret the server will never show again.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CopyableProps {
    pub label: AttrValue,
    pub value: AttrValue,

    #[prop_or(true)]
    pub monospace: bool,
}

/// A labelled value with a copy button beside it, shown in full: these are
/// URLs and tokens an operator compares character by character.
#[function_component(Copyable)]
pub fn copyable(props: &CopyableProps) -> Html {
    html! {
        <div class="copyable">
            <span class="copyable__label">{ props.label.clone() }</span>
            <yewi::Copyable
                value={props.value.clone()}
                variant={yewi::CopyableVariant::Field}
                mono={props.monospace}
                label={format!("Copy {}", props.label.to_lowercase())}
            />
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct SecretRevealProps {
    pub title: AttrValue,

    pub secret: AttrValue,

    /// The link a device opens to enrol with the secret, when there is one.
    #[prop_or_default]
    pub enroll_url: Option<AttrValue>,

    /// What belongs beside the values: a QR code of the link, say.
    #[prop_or_default]
    pub children: Html,

    pub ondismiss: Callback<()>,
}

/// A secret, shown once. It never closes on its own: one that vanished while
/// it was being typed in would have to be issued again.
#[function_component(SecretReveal)]
pub fn secret_reveal(props: &SecretRevealProps) -> Html {
    let warning = html! {
        { "This is the only time this secret is shown. The server kept a hash of it and \
           nothing else, so nobody — including an administrator — can show it again." }
    };

    html! {
        <yewi::SecretReveal
            title={props.title.clone()}
            {warning}
            aside={props.children.clone()}
            on_dismiss={props.ondismiss.clone()}
        >
            <Copyable label="Secret" value={props.secret.clone()} />
            if let Some(url) = &props.enroll_url {
                <Copyable label="Enrolment link" value={url.clone()} />
            }
        </yewi::SecretReveal>
    }
}
