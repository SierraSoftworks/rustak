//! How this server describes itself, and how you get back into it.
//!
//! The settings themselves are read-only here. Values set in `config.toml` win
//! over anything the wizard wrote, so a form that appeared to accept a change
//! the file then overrode would be lying; editing them arrives with the settings
//! API in a later milestone.
//!
//! The passkeys are not read-only, and deliberately so: the one thing worth
//! doing from this page today is registering a second way in before the first
//! one is lost.
//!
//! [`TlsCard`] and [`FilesCard`] carry the two things on this page worth
//! acting on: ordering a certificate when the last order failed, and the
//! upload ceiling, which is a limit rather than a suggestion because the same
//! number is advertised to clients and enforced on every upload.

use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, LoadingNote};
use crate::util::format_iso8601;

use super::load::{use_refresh_action, use_resource};
use super::panels::PasskeysPanel;
use super::settings_files::FilesCard;
use super::settings_tls::TlsCard;

#[function_component(Settings)]
pub fn settings() -> Html {
    let settings = use_resource(api::settings::get);
    use_refresh_action(settings.reload.clone(), settings.busy);

    let details = match (&settings.data, &settings.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load this server's settings."
                message={message.clone()}
            />
        },
        (Some(settings), _) => html! {
            <dl class="detail-list">
                <dt>{ "Name" }</dt>
                <dd>{ settings.name.clone() }</dd>
                <dt>{ "Host names" }</dt>
                <dd>
                    if settings.domains.is_empty() {
                        { "— not set —" }
                    } else {
                        { settings.domains.join(", ") }
                    }
                </dd>
                <dt>{ "Base URL" }</dt>
                <dd>
                    { settings.base_url.clone().unwrap_or_else(|| "— inferred —".to_string()) }
                </dd>
                <dt>{ "Node ID" }</dt>
                <dd>
                    <code>{ settings.node_id.clone().unwrap_or_else(|| "—".to_string()) }</code>
                </dd>
                <dt>{ "Set up" }</dt>
                <dd>
                    {
                        settings
                            .setup_completed_at
                            .map(format_iso8601)
                            .unwrap_or_else(|| "Not finished".to_string())
                    }
                </dd>
            </dl>
        },
    };

    html! {
        <>
            <Card
                title="This server"
                subtitle="Values set in config.toml win over the ones the wizard wrote, so \
                    this is what clients are actually told."
            >
                { details }
            </Card>

            <TlsCard />

            <FilesCard />

            <PasskeysPanel
                title="Your passkeys"
                subtitle="Registering a second one is how you keep a way in when the first \
                    device is lost. The rest of how you sign in is on the Credentials page."
            />
        </>
    }
}
