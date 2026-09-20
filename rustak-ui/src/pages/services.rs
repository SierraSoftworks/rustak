//! The sidecars registered with this server, and what each last reported.
//!
//! One row per registration: what it calls itself, what it can do, and the
//! state its last heartbeat carried. The question an operator opens this page
//! with is "is the feed still running?", so the answer is the pill and the
//! time since the last heartbeat, and everything that needs an explanation —
//! the endpoints, the metrics, the configuration — is in the drawer below.
//!
//! # Ordering and refreshing
//!
//! Anything needing attention sorts first, worst first within that, so a fleet
//! of twenty healthy plugins never hides the one that stopped. The list
//! re-reads itself every ten seconds while the tab is in front; a hidden tab
//! keeps the timer but skips the request, so a console left open overnight is
//! not polling all night. There is no event feed behind this — `GET /services`
//! is one cheap read, and the server's own health sweep is what moves a silent
//! service to *not reporting* in between.

use rustak_api::{ServiceState, ServiceSummary};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, LoadingNote, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative, window};

use super::load::{use_refresh_action, use_resource};
use super::service_detail::ServiceDetail;

/// How often the list re-reads itself while the tab is in front.
const REFRESH_MS: u32 = 10_000;

/// Where the documentation for registering a sidecar lives.
const DOCS: &str = "https://github.com/SierraSoftworks/rustak/blob/main/docs/plugins.md\
                    #registering-with-the-server";

/// The tone a service's state is drawn in.
///
/// *Not reporting* is a warning rather than a neutral: it is what the server's
/// health sweep moves a registration to when it goes quiet, so it means "this
/// was running and now says nothing", which is the opposite of neutral.
pub fn tone_of(state: ServiceState) -> StatusTone {
    match state {
        ServiceState::Healthy => StatusTone::Ok,
        ServiceState::Degraded | ServiceState::Unknown => StatusTone::Warning,
        ServiceState::Unhealthy => StatusTone::Error,
    }
}

/// How bad a state is, worst highest. [`ServiceState::ALL`] is ordered worst
/// last, so the position in it is the ranking — except that *not reporting*
/// sits at the start of that list while still needing attention, which is why
/// the sort below keys on [`ServiceState::needs_attention`] first.
fn severity(state: ServiceState) -> usize {
    ServiceState::ALL
        .iter()
        .position(|candidate| *candidate == state)
        .unwrap_or(0)
}

/// The order the list is drawn in: what needs attention first, worst first
/// within that, then by the name the row shows.
pub fn ordered(services: &[ServiceSummary]) -> Vec<ServiceSummary> {
    let mut ordered = services.to_vec();
    ordered.sort_by_key(|service| {
        let state = service.status.state;
        (
            !state.needs_attention(),
            std::cmp::Reverse(severity(state)),
            service.descriptor.display().to_lowercase(),
        )
    });

    ordered
}

/// Whether the tab is hidden behind another, so the poll can skip a request
/// nobody is looking at without stopping the timer that re-arms it.
fn tab_hidden() -> bool {
    window()
        .document()
        .map(|document| document.hidden())
        .unwrap_or(false)
}

#[function_component(Services)]
pub fn services() -> Html {
    let services = use_resource(api::services::list);
    use_refresh_action(services.reload.clone(), services.busy);

    let selected = use_state(|| None::<String>);
    let tick = use_state(|| 0u32);

    // A timeout re-armed after each render that is not busy, rather than an
    // interval: a timer that fires while a request is still in flight stacks
    // them up on a slow link. The tick is what re-arms it when the request was
    // skipped, which is the case a hidden tab is in.
    {
        let (reload, busy, tick) = (services.reload.clone(), services.busy, tick.clone());
        use_effect_with((busy, *tick), move |(busy, _)| {
            let handle = (!*busy).then(|| {
                gloo_timers::callback::Timeout::new(REFRESH_MS, move || {
                    if !tab_hidden() {
                        reload.emit(());
                    }
                    tick.set(*tick + 1);
                })
            });

            move || drop(handle)
        });
    }

    let body = match (&services.data, &services.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the registered services."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "Nothing has registered. A sidecar registers itself when its configuration \
                   names this server's control API, so there is nothing to add here by hand — " }
                <a href={DOCS} target="_blank" rel="noopener noreferrer">
                    { "see Registering with the server" }
                </a>
                { "." }
            </p>
        },
        (Some(list), _) => {
            let rows = ordered(list);
            html! {
                <ul class="service-list">
                    { for rows.into_iter().map(|service| {
                        let name = service.descriptor.name.to_string();
                        let chosen = selected.as_deref() == Some(name.as_str());
                        let onselect = {
                            let (selected, name) = (selected.clone(), name.clone());
                            Callback::from(move |_: MouseEvent| {
                                selected.set((selected.as_deref() != Some(name.as_str()))
                                    .then(|| name.clone()));
                            })
                        };

                        html! {
                            <li key={name.clone()}>{ row(&service, chosen, onselect) }</li>
                        }
                    }) }
                </ul>
            }
        }
    };

    let chosen = services.data.as_deref().and_then(|list| {
        list.iter()
            .find(|service| selected.as_deref() == Some(service.descriptor.name.as_str()))
            .cloned()
    });

    html! {
        <>
            <Card
                title="Registered services"
                subtitle="Every sidecar that has registered, and what its last heartbeat said."
            >
                if let Some(message) = &services.error {
                    <Alert
                        kind={AlertKind::Error}
                        title="We could not refresh the list."
                        message={message.clone()}
                    />
                }

                { body }
            </Card>

            if let Some(service) = chosen {
                // Keyed on the name so that choosing a different service
                // *remounts* the drawer. Without it Yew re-uses the instance,
                // and the drawer's own fetches — the configuration especially —
                // would still be showing the service that was open before.
                <ServiceDetail
                    key={service.descriptor.name.to_string()}
                    service={service.clone()}
                    on_removed={
                        let (selected, reload) = (selected.clone(), services.reload.clone());
                        Callback::from(move |_| {
                            selected.set(None);
                            reload.emit(());
                        })
                    }
                />
            }
        </>
    }
}

/// One registration's row.
fn row(service: &ServiceSummary, selected: bool, onselect: Callback<MouseEvent>) -> Html {
    let state = service.status.state;
    let descriptor = &service.descriptor;
    let heartbeat = service.status.last_heartbeat_at;

    html! {
        <div class={classes!(
            "service-row",
            selected.then_some("service-row--selected"),
            state.needs_attention().then_some("service-row--attention"),
        )}>
            <button type="button" class="service-row__select" onclick={onselect}>
                <span class="service-row__name">{ descriptor.display().to_string() }</span>
                <span class="service-row__id">{ descriptor.name.to_string() }</span>
            </button>

            <div class="service-row__meta">
                <span>{ descriptor.version.clone().unwrap_or_else(|| "—".to_string()) }</span>
                <span title={heartbeat.map(format_iso8601)}>
                    { match heartbeat {
                        Some(at) => format!("Heartbeat {}", short_relative(at)),
                        None => "Never reported".to_string(),
                    } }
                </span>
                <span title={format_iso8601(service.registered_at)}>
                    { format!("Registered {}", short_relative(service.registered_at)) }
                </span>
            </div>

            <div class="service-row__tags">
                { for descriptor.capabilities.iter().map(|capability| html! {
                    <span class="tag" key={capability.to_string()}>
                        { capability.to_string() }
                    </span>
                }) }
            </div>

            <StatusPill
                tone={tone_of(state)}
                label={state.label()}
                title={service.status.message.clone()}
            />

            if let Some(message) = &service.status.message {
                <p class="service-row__message">{ message.clone() }</p>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::{ServiceDescriptor, ServiceId, ServiceName, ServiceStatus};

    use super::*;

    fn service(name: &str, state: ServiceState) -> ServiceSummary {
        ServiceSummary {
            id: ServiceId::new(1),
            descriptor: ServiceDescriptor::new(ServiceName::from_storage(name)),
            status: ServiceStatus {
                state,
                message: None,
                last_heartbeat_at: None,
            },
            registered_at: chrono::Utc::now(),
            metrics: serde_json::Value::Null,
        }
    }

    #[test]
    fn what_needs_attention_sorts_first_worst_first() {
        let listed = [
            service("healthy-one", ServiceState::Healthy),
            service("quiet-one", ServiceState::Unknown),
            service("healthy-two", ServiceState::Healthy),
            service("degraded-one", ServiceState::Degraded),
            service("stopped-one", ServiceState::Unhealthy),
        ];

        let names: Vec<String> = ordered(&listed)
            .iter()
            .map(|service| service.descriptor.name.to_string())
            .collect();

        assert_eq!(
            names,
            vec![
                "stopped-one",
                "degraded-one",
                // Never reported: it needs attention, and it is the least bad
                // of the three that do.
                "quiet-one",
                "healthy-one",
                "healthy-two",
            ]
        );
    }

    #[test]
    fn a_state_that_needs_attention_is_never_drawn_as_ok() {
        for state in ServiceState::ALL.iter().copied() {
            let tone = tone_of(state);
            assert_eq!(
                state.needs_attention(),
                tone != StatusTone::Ok,
                "{state:?} is drawn as {tone:?}"
            );
        }
    }
}
