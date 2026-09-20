//! Demo data and behaviour for the registered sidecars.
//!
//! Three services, because three is what the Services page has to be able to
//! draw at once: one healthy feed reporting the counters M9's feed sidecars
//! report, one degraded feed that is reconnecting and says so, and one that
//! stopped reporting long enough ago that the server's health sweep moved it
//! back to *not reporting*. Between them they cover every pill, both halves of
//! the ordering, and a `metrics` object with a nested value in it.
//!
//! The configuration belongs to the AIS feed, so the Configuration panel has
//! something to show, edit and save without a server behind it.

use std::cell::RefCell;

use rustak_api::{
    Capability, ServiceDescriptor, ServiceEndpoints, ServiceId, ServiceName, ServiceState,
    ServiceStatus, ServiceSummary,
};

use super::data::ago;
use crate::api::ApiError;

struct State {
    services: Vec<ServiceSummary>,
    configs: Vec<(String, serde_json::Value)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

fn with<R>(action: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| action(&mut state.borrow_mut()))
}

fn missing() -> ApiError {
    ApiError::Server("No service is registered under that name.".to_string())
}

fn descriptor(
    name: &str,
    display: &str,
    version: &str,
    capabilities: &[&str],
) -> ServiceDescriptor {
    ServiceDescriptor {
        name: ServiceName::from_storage(name),
        display_name: Some(display.to_string()),
        version: Some(version.to_string()),
        capabilities: capabilities.iter().map(Capability::from_storage).collect(),
        endpoints: ServiceEndpoints {
            stream: Some("ssl://rustak:8089".to_string()),
            marti: Some("https://rustak:8443".to_string()),
            control: Some("https://rustak:8446".to_string()),
        },
    }
}

impl State {
    fn new() -> Self {
        Self {
            services: vec![
                // Healthy, and reporting the counters a feed sidecar reports:
                // what the upstream offered, what was published after the
                // policy knobs had their say, and where the feed itself is.
                ServiceSummary {
                    id: ServiceId::new(41),
                    descriptor: descriptor(
                        "rustak-plugin-ais",
                        "AIS feed",
                        "0.1.0",
                        &["cot.publish", "feed.ais"],
                    ),
                    status: ServiceStatus {
                        state: ServiceState::Healthy,
                        message: None,
                        last_heartbeat_at: Some(ago(1)),
                    },
                    registered_at: ago(310),
                    metrics: serde_json::json!({
                        "offered": 18_422,
                        "published": 4_106,
                        "suppressed": 14_291,
                        "expired": 25,
                        "tracked": 612,
                        "source": { "kind": "aisstream", "state": "connected" },
                    }),
                },
                // Degraded: still registered, still publishing what it has,
                // but its upstream dropped and it is backing off.
                ServiceSummary {
                    id: ServiceId::new(42),
                    descriptor: descriptor(
                        "rustak-plugin-adsb",
                        "ADS-B feed",
                        "0.1.0",
                        &["cot.publish", "feed.adsb"],
                    ),
                    status: ServiceStatus {
                        state: ServiceState::Degraded,
                        message: Some(
                            "The upstream feed has not answered for 4 minutes. Retrying every \
                             30 seconds."
                                .to_string(),
                        ),
                        last_heartbeat_at: Some(ago(2)),
                    },
                    registered_at: ago(295),
                    metrics: serde_json::json!({
                        "offered": 9_140,
                        "published": 2_233,
                        "suppressed": 6_804,
                        "expired": 103,
                        "tracked": 87,
                        "source": { "kind": "readsb", "state": "reconnecting" },
                    }),
                },
                // Stopped reporting. `plugins::health::sweep` moves a
                // registration that has been quiet for its grace period back to
                // a state that needs attention, which is what this is.
                ServiceSummary {
                    id: ServiceId::new(43),
                    descriptor: descriptor(
                        "rustak-plugin-example",
                        "Example sidecar",
                        "0.1.0",
                        &["cot.publish"],
                    ),
                    status: ServiceStatus {
                        state: ServiceState::Unhealthy,
                        message: Some(
                            "No heartbeat for 47 minutes. The process may have stopped."
                                .to_string(),
                        ),
                        last_heartbeat_at: Some(ago(47)),
                    },
                    registered_at: ago(1_440),
                    metrics: serde_json::json!({ "offered": 0, "published": 0 }),
                },
            ],
            configs: vec![(
                "rustak-plugin-ais".to_string(),
                serde_json::json!({
                    "interval_seconds": 30,
                    "bounding_box": [-11.0, 49.5, 2.5, 61.0],
                    "min_speed_knots": 0.5,
                }),
            )],
        }
    }
}

/// Every registered service.
pub fn services() -> Vec<ServiceSummary> {
    with(|state| state.services.clone())
}

/// One service's configuration, or `{}` for a service nobody has configured —
/// which is what the server answers, rather than a refusal.
pub fn service_config(name: &str) -> Result<serde_json::Value, ApiError> {
    with(|state| {
        if !state
            .services
            .iter()
            .any(|service| service.descriptor.name.as_str() == name)
        {
            return Err(missing());
        }

        Ok(state
            .configs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, config)| config.clone())
            .unwrap_or_else(|| serde_json::json!({})))
    })
}

/// Replaces a service's configuration and answers with what was stored, so the
/// panel reloads from the same place a real save would.
pub fn set_service_config(
    name: &str,
    config: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    with(|state| {
        if !state
            .services
            .iter()
            .any(|service| service.descriptor.name.as_str() == name)
        {
            return Err(missing());
        }

        match state.configs.iter_mut().find(|(key, _)| key == name) {
            Some((_, stored)) => *stored = config.clone(),
            None => state.configs.push((name.to_string(), config.clone())),
        }

        Ok(config.clone())
    })
}

/// Removes a registration, and the configuration that went with it.
pub fn remove_service(name: &str) -> Result<(), ApiError> {
    with(|state| {
        let before = state.services.len();
        state
            .services
            .retain(|service| service.descriptor.name.as_str() != name);

        if state.services.len() == before {
            return Err(missing());
        }

        state.configs.retain(|(key, _)| key != name);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_fixtures_cover_the_states_the_page_draws() {
        let services = State::new().services;

        assert_eq!(services.len(), 3);
        let states: Vec<ServiceState> = services.iter().map(|it| it.status.state).collect();
        assert_eq!(
            states,
            vec![
                ServiceState::Healthy,
                ServiceState::Degraded,
                ServiceState::Unhealthy
            ]
        );
        // Two of the three need attention, so the list has both halves of its
        // ordering to sort and the dashboard tile has a non-zero count.
        assert_eq!(
            services
                .iter()
                .filter(|it| it.status.state.needs_attention())
                .count(),
            2
        );
    }

    #[test]
    fn the_feed_metrics_have_a_nested_object_in_them() {
        let services = State::new().services;
        let ais = &services[0];

        assert_eq!(ais.metrics["offered"], serde_json::json!(18_422));
        assert_eq!(
            ais.metrics["source"]["state"],
            serde_json::json!("connected")
        );
        assert_eq!(
            services[1].metrics["source"]["state"],
            serde_json::json!("reconnecting")
        );
    }

    #[test]
    fn a_configuration_is_stored_and_read_back_and_a_removal_takes_it_with_it() {
        // Against a fresh `State` rather than the thread-local, so the test
        // says nothing about the order the tests happen to run in.
        let mut state = State::new();
        let name = "rustak-plugin-ais";

        assert_eq!(state.configs.len(), 1);
        state
            .configs
            .iter_mut()
            .find(|(key, _)| key == name)
            .map(|(_, stored)| *stored = serde_json::json!({ "interval_seconds": 5 }))
            .expect("the AIS feed is configured");
        assert_eq!(
            state.configs[0].1,
            serde_json::json!({ "interval_seconds": 5 })
        );

        state
            .services
            .retain(|service| service.descriptor.name.as_str() != name);
        state.configs.retain(|(key, _)| key != name);
        assert_eq!(state.services.len(), 2);
        assert!(state.configs.is_empty());
    }
}
