//! Your own sign-in methods, devices and credentials.
//!
//! Enrolling a phone is something a person should be able to do for themselves:
//! `POST /api/v1/credentials` with no `username` mints for the caller, and
//! `GET /api/v1/devices` answers anybody who is not an administrator with their
//! own. So this page needs no administrative access at all — it is the same two
//! panels an administrator sees on somebody else's account, pointed at nobody in
//! particular.

use yew::prelude::*;

use crate::app::AuthHandle;
use crate::components::{Alert, AlertKind, Card, LoadingNote};

use super::panels::{CredentialsPanel, DevicesPanel, PasskeysPanel, SignInMethods};

#[function_component(Me)]
pub fn me() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let Some(user) = auth.user.clone() else {
        // `Protected` has already resolved the session by the time this mounts,
        // so this is the moment between the two rather than a failure.
        return html! { <LoadingNote /> };
    };

    html! {
        <>
            <Card
                title={format!("Signed in as {}", user.display_name.clone().unwrap_or_else(|| user.username.to_string()))}
                subtitle="Bring a client on without asking an administrator."
            >
                <Alert
                    kind={AlertKind::Info}
                    title="Enrolling a device"
                    message="Mint an enrolment token below, then scan the QR code from ATAK's \
                        Quick Connect — Settings → Network Preferences → Manage Server \
                        Connections. The token is spent the moment a certificate is issued, \
                        and it is worthless afterwards."
                />
            </Card>

            <SignInMethods user={user.clone()} on_changed={auth.refresh.clone()} />

            <PasskeysPanel
                title="Your passkeys"
                subtitle="The local way in. Register one on each device you sign in from."
            />

            // No username: the server reads the caller's own, which is the whole
            // point — a person does not need administrative access to do this.
            <CredentialsPanel title="Your credentials" />
            <DevicesPanel title="Your devices" />
        </>
    }
}
