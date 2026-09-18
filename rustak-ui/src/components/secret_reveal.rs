//! A secret the server will never say again.
//!
//! Minting a credential is the one response that carries its secret; the server
//! kept only an argon2 hash, so there is no endpoint that could show it a second
//! time. That makes this panel the last place the value exists outside the
//! client that will use it, and everything about it follows from that: it says
//! so in as many words, it offers a copy button because retyping a 32-character
//! token is how people end up writing it down, and it does not disappear on its
//! own.
//!
//! Nothing here is logged, and the value reaches the DOM only as a text node.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::components::{Button, ButtonKind};
use crate::util::copy_to_clipboard;

#[derive(Properties, PartialEq)]
pub struct CopyableProps {
    /// What this value is, above the box.
    pub label: AttrValue,

    pub value: AttrValue,

    /// Renders the value in a monospaced face and lets it wrap anywhere, which
    /// is what a token or a URL needs.
    #[prop_or(true)]
    pub monospace: bool,
}

/// One value with a button that puts it on the clipboard.
#[function_component(Copyable)]
pub fn copyable(props: &CopyableProps) -> Html {
    let state = use_state(|| None::<Result<(), String>>);

    let oncopy = {
        let (state, value) = (state.clone(), props.value.to_string());
        Callback::from(move |_: MouseEvent| {
            let (state, value) = (state.clone(), value.clone());
            spawn_local(async move { state.set(Some(copy_to_clipboard(&value).await)) });
        })
    };

    let note = match &*state {
        Some(Ok(())) => Some(html! { <span class="copyable__note">{ "Copied" }</span> }),
        Some(Err(message)) => Some(html! {
            <span class="copyable__note copyable__note--error" role="alert">
                { message.clone() }
            </span>
        }),
        None => None,
    };

    html! {
        <div class="copyable">
            <div class="copyable__header">
                <span class="copyable__label">{ props.label.clone() }</span>
                { note.unwrap_or_default() }
            </div>
            <div class="copyable__row">
                <code class={classes!(
                    "copyable__value",
                    props.monospace.then_some("copyable__value--mono"),
                )}>
                    { props.value.clone() }
                </code>
                <Button small=true onclick={oncopy} title="Copy to the clipboard">
                    { "Copy" }
                </Button>
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct SecretRevealProps {
    /// The heading, which should name what was minted.
    pub title: AttrValue,

    /// The secret itself.
    pub secret: AttrValue,

    /// The `tak://` URL, when the credential is one a client enrols with. It
    /// embeds the secret, so it is exactly as sensitive.
    #[prop_or_default]
    pub enroll_url: Option<AttrValue>,

    /// Anything the page wants under the values — a QR code, a warning, or the
    /// instructions for the client this is for.
    #[prop_or_default]
    pub children: Html,

    /// Dismisses the panel. Only ever driven by somebody clicking, never by a
    /// timer: a secret that vanished while it was being typed in would have to
    /// be minted again.
    pub ondismiss: Callback<()>,
}

/// The panel a freshly minted secret is shown in, once.
#[function_component(SecretReveal)]
pub fn secret_reveal(props: &SecretRevealProps) -> Html {
    let ondismiss = {
        let ondismiss = props.ondismiss.clone();
        Callback::from(move |_: MouseEvent| ondismiss.emit(()))
    };

    html! {
        <section class="secret-reveal" role="region" aria-label={props.title.clone()}>
            <header class="secret-reveal__header">
                <div>
                    <h3 class="secret-reveal__title">{ props.title.clone() }</h3>
                    <p class="secret-reveal__warning">
                        { "This is the only time this secret is shown. The server kept a hash \
                           of it and nothing else, so nobody — including an administrator — \
                           can show it again." }
                    </p>
                </div>
                <Button kind={ButtonKind::Subtle} small=true onclick={ondismiss}>{ "Done" }</Button>
            </header>

            <div class="secret-reveal__body">
                <div class="secret-reveal__values">
                    <Copyable label="Secret" value={props.secret.clone()} />
                    if let Some(url) = &props.enroll_url {
                        <Copyable label="Enrolment link" value={url.clone()} />
                    }
                </div>
                { props.children.clone() }
            </div>
        </section>
    }
}
