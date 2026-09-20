//! How this server is reached, and how people prove who they are.
//!
//! Three cards. What the server calls itself is read-only here: values set in
//! `config.toml` win over anything the wizard wrote, so a form that appeared
//! to accept a change the file then overrode would be lying. The transport
//! card carries the one thing worth doing — reloading a certificate whose
//! last fetch failed — and the authentication card says how sign-in is set
//! up, from the same metadata the login page draws itself with.

use rustak_api::{AuthMetadata, AuthMode};
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{Alert, AlertKind, Card, LoadingNote, StatusPill, StatusTone};
use crate::util::format_iso8601;

use super::load::{use_refresh_action, use_resource};
use super::settings_tls::TlsCard;

#[function_component(Security)]
pub fn security() -> Html {
    let settings = use_resource(api::settings::get);
    let metadata = use_resource(api::auth::metadata);

    let reload = {
        let reloads = [settings.reload.clone(), metadata.reload.clone()];
        Callback::from(move |_| {
            for reload in &reloads {
                reload.emit(());
            }
        })
    };
    use_refresh_action(reload, settings.busy || metadata.busy);

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

    let auth = match (&metadata.data, &metadata.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read how sign-in is configured."
                message={message.clone()}
            />
        },
        (Some(metadata), _) => html! { <AuthDetails metadata={metadata.clone()} /> },
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

            <Card
                title="Authentication"
                subtitle="How people sign in to this console, and how you signed in."
            >
                { auth }
            </Card>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct AuthDetailsProps {
    metadata: AuthMetadata,
}

/// The sign-in arrangement, and the signed-in person's own path through it.
#[function_component(AuthDetails)]
fn auth_details(props: &AuthDetailsProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let metadata = &props.metadata;

    let (mode_label, mode_tone) = match metadata.mode {
        AuthMode::Oidc { .. } => ("Single sign-on", StatusTone::Ok),
        AuthMode::Passkey => ("Passkeys only", StatusTone::Neutral),
    };

    html! {
        <>
            <div class="card__status">
                <StatusPill tone={mode_tone} label={mode_label} />
                <StatusPill
                    tone={if metadata.passkeys_enabled { StatusTone::Ok } else { StatusTone::Neutral }}
                    label={if metadata.passkeys_enabled { "Passkeys accepted" } else { "Passkeys off" }}
                    title="A passkey registered here is a way in when the provider is unreachable."
                />
            </div>

            <dl class="detail-list">
                if let AuthMode::Oidc { authorization_endpoint, client_id, scopes, pkce } = &metadata.mode {
                    <dt>{ "Identity provider" }</dt>
                    <dd><code>{ authorization_endpoint.clone() }</code></dd>
                    <dt>{ "Client ID" }</dt>
                    <dd><code>{ client_id.clone() }</code></dd>
                    <dt>{ "Scopes" }</dt>
                    <dd>
                        { match scopes.is_empty() {
                            true => "— none —".to_string(),
                            false => scopes.join(" "),
                        } }
                    </dd>
                    <dt>{ "PKCE" }</dt>
                    <dd>{ if *pkce { "Required" } else { "Not used" } }</dd>
                }

                if let Some(me) = &auth.user {
                    <dt>{ "You signed in" }</dt>
                    <dd>{ me.via.label() }</dd>
                    if let Some(provider) = &me.identity_provider {
                        <dt>{ "Your provider" }</dt>
                        <dd>{ provider.clone() }</dd>
                    }
                    <dt>{ "Your access" }</dt>
                    <dd>{ if me.is_admin { "Administrator" } else { "Member" } }</dd>
                }
            </dl>

            // The access-control rules, the claim mappings and the claims your
            // sign-in recorded are judged on the server but not served by it
            // yet; this card grows to show them once they are.
            <p class="panel-note">
                { "Access-control rules and recorded claims are not yet readable from here." }
            </p>
        </>
    }
}
