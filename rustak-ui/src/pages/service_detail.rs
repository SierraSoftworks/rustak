//! One registered sidecar: what it says about itself, what it last reported,
//! and what an administrator can do about it.
//!
//! A drawer beside the list rather than a page of its own, the way the
//! situation browser's does it: everything here is about a row in a list that
//! keeps refreshing behind it.
//!
//! Three things the row has no space for live here. The **endpoints** are what
//! the sidecar says it reached us on, which is what makes a plugin talking to
//! the wrong address diagnosable at all. The **metrics** are whatever the
//! plugin counts, rendered as a table by [`super::service_metrics`]. And
//! **Remove** takes the registration away — not the account and not the
//! certificate, so a sidecar that is merely stopped can be tidied out of the
//! listing and will register again when it comes back.

use rustak_api::{ServiceEndpoints, ServiceSummary};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, ConfirmButton, DetailDrawer, StatusPill, Tag};
use crate::util::{format_iso8601, optional_relative, short_relative};

use super::service_config::ServiceConfigPanel;
use super::service_metrics::MetricsTable;
use super::services::tone_of;

#[derive(Properties, PartialEq)]
pub struct ServiceDetailProps {
    pub service: ServiceSummary,

    /// Called once the registration has gone, so the list can drop the row and
    /// the drawer can close.
    pub on_removed: Callback<()>,

    pub on_close: Callback<()>,
}

#[function_component(ServiceDetail)]
pub fn service_detail(props: &ServiceDetailProps) -> Html {
    let service = &props.service;
    let name = service.descriptor.name.to_string();

    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let remove = {
        let name = name.clone();
        let (busy, error, on_removed) = (busy.clone(), error.clone(), props.on_removed.clone());

        Callback::from(move |_| {
            let name = name.clone();
            let (busy, error, on_removed) = (busy.clone(), error.clone(), on_removed.clone());

            busy.set(true);
            spawn_local(async move {
                match api::services::remove(&name).await {
                    Ok(()) => {
                        error.set(None);
                        on_removed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let state = service.status.state;
    let descriptor = &service.descriptor;

    let actions = html! {
        <ConfirmButton
            small=false
            label="Remove"
            confirm_label="Remove it"
            question={format!(
                "Remove the registration for '{name}'? Its account, certificate and channels \
                 are left alone, and a sidecar that is still running will register again on \
                 its next heartbeat.",
            )}
            busy={*busy}
            onconfirm={remove}
        />
    };

    html! {
        <DetailDrawer
            wide=true
            title={descriptor.display().to_string()}
            subtitle="What this sidecar reads, what it says about itself, and what it last reported."
            {actions}
            on_close={props.on_close.clone()}
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That registration could not be removed."
                    message={message.clone()}
                />
            }

            <ServiceConfigPanel name={name.clone()} schema={descriptor.config_schema.clone()} />

            <section class="drawer-section" aria-label="Registration">
                <h3 class="drawer-section__title">
                    { "Registration" }
                    <StatusPill tone={tone_of(state)} label={state.label()} />
                </h3>

                <dl class="detail-list">
                    <dt>{ "Name" }</dt>
                    <dd><code>{ name.clone() }</code></dd>

                    <dt>{ "Version" }</dt>
                    <dd>{ descriptor.version.clone().unwrap_or_else(|| "—".to_string()) }</dd>

                    <dt>{ "State" }</dt>
                    <dd>{ service.status.message.clone().unwrap_or_else(||
                        state.label().to_string()) }</dd>

                    <dt>{ "Last heartbeat" }</dt>
                    <dd title={service.status.last_heartbeat_at.map(format_iso8601)}>
                        { optional_relative(service.status.last_heartbeat_at) }
                    </dd>

                    <dt>{ "Registered" }</dt>
                    <dd title={format_iso8601(service.registered_at)}>
                        { short_relative(service.registered_at) }
                    </dd>

                    <dt>{ "Capabilities" }</dt>
                    <dd>
                        if descriptor.capabilities.is_empty() {
                            { "— none advertised —" }
                        } else {
                            <span class="detail-list__pills">
                                { for descriptor.capabilities.iter().map(|capability| html! {
                                    <Tag key={capability.to_string()} mono=true>
                                        { capability.to_string() }
                                    </Tag>
                                }) }
                            </span>
                        }
                    </dd>

                    { endpoints(&descriptor.endpoints) }
                </dl>
            </section>

            <section class="drawer-section" aria-label="Metrics">
                <h3 class="drawer-section__title">{ "Metrics" }</h3>
                <p class="panel-note">
                    { "Whatever this plugin counts, as its last heartbeat reported it." }
                </p>
                <MetricsTable metrics={service.metrics.clone()} />
            </section>
        </DetailDrawer>
    }
}

/// The addresses the sidecar says it reached this server on.
///
/// Reported rather than assigned, so a plugin in the same compose file and one
/// across a network show different values — and a plugin pointed at the wrong
/// address shows the wrong one, which is the whole point of listing them.
fn endpoints(endpoints: &ServiceEndpoints) -> Html {
    let row = |label: &'static str, value: &Option<String>| match value {
        Some(value) => html! {
            <>
                <dt>{ label }</dt>
                <dd><code>{ value.clone() }</code></dd>
            </>
        },
        None => Html::default(),
    };

    if endpoints == &ServiceEndpoints::default() {
        return html! {
            <>
                <dt>{ "Endpoints" }</dt>
                <dd>{ "— none reported —" }</dd>
            </>
        };
    }

    html! {
        <>
            { row("Stream endpoint", &endpoints.stream) }
            { row("Marti endpoint", &endpoints.marti) }
            { row("Control endpoint", &endpoints.control) }
        </>
    }
}
