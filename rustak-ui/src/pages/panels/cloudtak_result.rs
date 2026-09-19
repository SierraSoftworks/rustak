//! What one CloudTAK hand-over produced: three URLs, two secrets and a file.
//!
//! Everything on this view exists exactly once. The password and the passphrase
//! are the only copies outside the client that will use them, and the keystore
//! is deleted by the server as it answers the download — so nothing here
//! disappears on a timer, and the failure of a download is shown rather than
//! swallowed.
//!
//! A `410` on the download is the interesting failure, and
//! [`crate::api::cloudtak::p12`] turns it into a sentence before it reaches
//! here, so this view has one error path rather than two.

use rustak_api::{CloudTakOnboarding, Username};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Copyable, SecretReveal};

use super::super::load::use_download;

#[derive(Properties, PartialEq)]
pub struct CloudTakResultProps {
    pub username: Username,
    pub onboarding: CloudTakOnboarding,

    /// Clears the result so another hand-over can be prepared. Never a timer:
    /// a passphrase that vanished while it was being pasted into CloudTAK
    /// would mean starting again.
    pub ondismiss: Callback<()>,
}

#[function_component(CloudTakResult)]
pub fn cloudtak_result(props: &CloudTakResultProps) -> Html {
    let onboarding = props.onboarding.clone();

    let download = {
        let (url, username) = (onboarding.p12_download_url.clone(), props.username.clone());

        use_download(move || {
            let (url, username) = (url.clone(), username.clone());
            async move { api::cloudtak::p12(&url, &username).await }
        })
    };

    html! {
        <>
            <SecretReveal
                title={format!("CloudTAK — {}", props.username)}
                secret={password(&onboarding)}
                ondismiss={props.ondismiss.clone()}
            >
                <div class="cloudtak-result">
                    <Copyable label="Username" value={props.username.to_string()} />
                    <Copyable label="Stream URL" value={onboarding.urls.stream.clone()} />
                    <Copyable label="API URL" value={onboarding.urls.api.clone()} />
                    <Copyable label="WebTAK URL" value={onboarding.urls.webtak.clone()} />
                    <Copyable label="Keystore passphrase" value={onboarding.p12_password.clone()} />
                </div>
            </SecretReveal>

            if onboarding.password.is_none() {
                <Alert
                    kind={AlertKind::Info}
                    title="No password is shown, because none was minted."
                    message="You reused a client password this account already had. The server \
                             kept only a hash of it, so it cannot be shown again — use the one \
                             you saved when it was minted."
                />
            }

            if let Some(message) = &download.error {
                <Alert
                    kind={AlertKind::Error}
                    title="That keystore did not arrive."
                    message={message.clone()}
                />
            }

            <div class="cloudtak-download">
                <p class="panel-note">
                    { "The keystore downloads once and is then deleted from the server, whether \
                       or not you saved it. Upload it to CloudTAK's Configure Server page with \
                       the passphrase above, alongside the username and password." }
                </p>

                <Button
                    kind={ButtonKind::Primary}
                    busy={download.busy}
                    onclick={
                        let start = download.start.clone();
                        Callback::from(move |_: MouseEvent| start.emit(()))
                    }
                >
                    { "Download keystore (.p12)" }
                </Button>
            </div>

            <Alert
                kind={AlertKind::Warning}
                title="The key was generated on the server for this download and discarded."
                message="This is the one exception to rustak never holding a device's private \
                         key. It was never written down unencrypted, it is gone as soon as the \
                         file is collected, and the certificate it belongs to can be revoked \
                         from the Certificates list like any other."
            />
        </>
    }
}

/// What the reveal shows as "the secret".
///
/// The password when one was minted; otherwise the passphrase, which is then
/// the only thing on the page that will never be said again. A reveal with
/// nothing in it would be a reveal claiming a secret it does not have.
fn password(onboarding: &CloudTakOnboarding) -> AttrValue {
    onboarding
        .password
        .clone()
        .unwrap_or_else(|| onboarding.p12_password.clone())
        .into()
}
