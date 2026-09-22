//! Asking a service whether a candidate configuration is one it can use.
//!
//! The schema a service registered says what *shape* its configuration takes;
//! it cannot say that an API key is one the upstream accepts. Only the service
//! can, so an administrator's candidate is put to it — when there is a way to.
//!
//! # The transport is the feed the sidecar already holds
//!
//! A sidecar dials this server and never the other way round, so there is no
//! address to call. What there is, is `GET /api/v1/events`: a request is
//! published on it to that service alone, the service reads the candidate back
//! through [`Validations::candidate`] and answers through
//! [`Validations::answer`], and the administrator's request — which has been
//! waiting on a `oneshot` all along — carries the answer home. See
//! `rustak_api::service_config` for the wire contract.
//!
//! # No transport is an answer, not an error
//!
//! A service that does not advertise `config.validate`, has no feed open, or
//! does not answer within [`ANSWER_WITHIN`] leaves the schema's verdict
//! standing, and [`ServiceCheck`] says which of those it was. A sidecar being
//! down must never be what stops an administrator fixing its configuration.
//!
//! # The candidate never crosses the bus
//!
//! It may hold a secret, and the bus keeps a ring of what it carried. The event
//! holds an id; the candidate lives here, for as long as somebody is waiting
//! for the answer and no longer, and is never logged.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rustak_api::{
    ConfigIssue, ConfigValidation, ConfigValidationReport, ConfigValidationRequest, ServiceCheck,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::db::repos::ServiceRow;
use crate::prelude::*;

use super::config_schema;

/// How long a service is given to answer.
///
/// Long enough for a plugin to try a credential against its upstream, short
/// enough that an administrator pressing Save is still looking at the page.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// How many candidates may be awaiting an answer at once.
///
/// Each one is a request body held in memory; an administrator session in a
/// loop must not be a way to hold an unbounded number of them.
const MAX_PENDING: usize = 32;

/// How much of a service's answer is passed on. It is rendered in the admin UI,
/// and a plugin with more to say than this has a bug rather than a finding.
const MAX_ISSUES: usize = 32;
const MAX_TEXT: usize = 512;

/// The candidates somebody is waiting for an answer about.
#[derive(Clone, Default)]
pub struct Validations {
    pending: Arc<Mutex<HashMap<Uuid, Pending>>>,
}

struct Pending {
    service: ServiceName,
    config: serde_json::Value,
    answer: Option<oneshot::Sender<ConfigValidation>>,
}

/// One open question, withdrawn when this is dropped — answered, timed out, or
/// abandoned because the administrator's own request went away.
struct Asked {
    id: Uuid,
    answer: oneshot::Receiver<ConfigValidation>,
    validations: Validations,
}

impl Drop for Asked {
    fn drop(&mut self) {
        self.validations.pending.lock().remove(&self.id);
    }
}

impl Validations {
    /// Opens a question, unless too many are open already.
    fn ask(&self, service: &ServiceName, config: serde_json::Value) -> Option<Asked> {
        let mut pending = self.pending.lock();

        if pending.len() >= MAX_PENDING {
            return None;
        }

        let id = Uuid::new_v4();
        let (sender, answer) = oneshot::channel();
        pending.insert(
            id,
            Pending {
                service: service.clone(),
                config,
                answer: Some(sender),
            },
        );

        Some(Asked {
            id,
            answer,
            validations: self.clone(),
        })
    }

    /// The candidate `service` was asked about under `id`.
    ///
    /// [`None`] for an id that is not open **or is another service's**: the two
    /// are the same answer on purpose, as everywhere else in the control API.
    pub fn candidate(&self, service: &ServiceName, id: Uuid) -> Option<ConfigValidationRequest> {
        self.pending
            .lock()
            .get(&id)
            .filter(|pending| &pending.service == service)
            .map(|pending| ConfigValidationRequest {
                id,
                config: pending.config.clone(),
            })
    }

    /// Records what `service` said about `id`, answering whether anybody was
    /// still waiting to hear it.
    pub fn answer(&self, service: &ServiceName, id: Uuid, validation: ConfigValidation) -> bool {
        let sender = self
            .pending
            .lock()
            .get_mut(&id)
            .filter(|pending| &pending.service == service)
            .and_then(|pending| pending.answer.take());

        sender.is_some_and(|sender| sender.send(validation).is_ok())
    }
}

impl std::fmt::Debug for Validations {
    /// Counts, never contents: a candidate may hold a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Validations")
            .field("pending", &self.pending.lock().len())
            .finish()
    }
}

/// Everything that can be said about `config` as a configuration for `row`.
#[instrument("plugins.validate_config", skip_all, fields(service = %row.name))]
pub async fn validate(
    services: &impl Services,
    row: &ServiceRow,
    config: &serde_json::Value,
) -> ConfigValidationReport {
    validate_within(services, row, config, ANSWER_WITHIN).await
}

/// [`validate`], with a wait of the caller's choosing so that the timeout can
/// be tested without sitting through it.
pub async fn validate_within(
    services: &impl Services,
    row: &ServiceRow,
    config: &serde_json::Value,
    within: Duration,
) -> ConfigValidationReport {
    let issues = config_schema::issues(row.config_schema.as_ref(), config);

    // The schema is the service's own statement of what it can read, so a
    // candidate it refuses is one the service would only refuse again.
    if !issues.is_empty() {
        return ConfigValidationReport::new(issues, ServiceCheck::Skipped);
    }

    let advertised = row
        .capabilities
        .iter()
        .any(|capability| capability.as_str() == rustak_api::CONFIG_VALIDATE);

    if !advertised {
        return ConfigValidationReport::new(Vec::new(), ServiceCheck::NotSupported);
    }

    // Known before asking, so that a sidecar that is simply not running costs
    // an administrator nothing rather than the whole timeout.
    if !services.events().is_attached(&row.name) {
        return ConfigValidationReport::new(Vec::new(), ServiceCheck::Unreachable);
    }

    let Some(mut asked) = services.validations().ask(&row.name, config.clone()) else {
        warn!("Too many configuration validations are already waiting; not asking the service.");

        return ConfigValidationReport::new(Vec::new(), ServiceCheck::Unreachable);
    };

    services.events().config_validation(&row.name, asked.id);

    match tokio::time::timeout(within, &mut asked.answer).await {
        Ok(Ok(validation)) => {
            ConfigValidationReport::new(bounded(validation.issues), ServiceCheck::Checked)
        }
        _ => {
            debug!("The service did not answer a configuration validation in time.");

            ConfigValidationReport::new(Vec::new(), ServiceCheck::Unreachable)
        }
    }
}

/// A service's issues, cut down to what is worth showing.
fn bounded(issues: Vec<ConfigIssue>) -> Vec<ConfigIssue> {
    issues
        .into_iter()
        .take(MAX_ISSUES)
        .map(|issue| ConfigIssue {
            path: issue.path.map(|path| truncated(&path)),
            message: truncated(&issue.message),
        })
        .collect()
}

fn truncated(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

#[cfg(test)]
mod tests {
    use rustak_api::event::ServerEventPayload;
    use rustak_api::{CONFIG_VALIDATE, Capability};

    use super::*;
    use crate::db::repos::{NewService, NewUser};

    const SOON: Duration = Duration::from_millis(50);

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "api_key": { "type": "string" } },
            "additionalProperties": false,
        })
    }

    async fn registered(context: &AppContext, name: &str, capabilities: &[&str]) -> ServiceRow {
        let account = context
            .db()
            .users()
            .create(NewUser::service(
                Username::parse(&format!("svc.{name}")).unwrap(),
            ))
            .await
            .unwrap();

        context
            .db()
            .services()
            .register(NewService {
                capabilities: capabilities
                    .iter()
                    .map(|name| Capability::parse(name).unwrap())
                    .collect(),
                config_schema: Some(schema()),
                ..NewService::new(ServiceName::parse(name).unwrap(), account.id)
            })
            .await
            .unwrap()
    }

    /// A sidecar's half of the exchange: waits for the request on the bus, reads
    /// the candidate back, and answers with `issues`.
    fn answering(context: &AppContext, row: &ServiceRow, issues: Vec<ConfigIssue>) {
        let (context, name) = (context.clone(), row.name.clone());
        let mut events = context.events().subscribe();

        tokio::spawn(async move {
            while let Ok(published) = events.recv().await {
                let ServerEventPayload::ConfigValidationRequested(asked) = &published.event.payload
                else {
                    continue;
                };

                let candidate = context.validations().candidate(&name, asked.request_id);
                assert!(candidate.is_some(), "the candidate is there to be read");
                context.validations().answer(
                    &name,
                    asked.request_id,
                    ConfigValidation::rejected(issues.clone()),
                );
            }
        });
    }

    #[tokio::test]
    async fn what_the_schema_refuses_is_never_put_to_the_service() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let row = registered(&context, "weather", &[CONFIG_VALIDATE]).await;
        let _feed = context.events().attach(&row.name);

        let report =
            validate_within(&context, &row, &serde_json::json!({ "api_kee": "x" }), SOON).await;

        assert!(!report.valid);
        assert_eq!(report.service, ServiceCheck::Skipped);
        assert_eq!(context.events().latest_id(), 0, "nothing was published");
    }

    #[tokio::test]
    async fn a_service_that_cannot_be_asked_leaves_the_schemas_verdict_standing() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let config = serde_json::json!({ "api_key": "x" });

        for (name, capabilities, attached, expected) in [
            ("plain", &[][..], true, ServiceCheck::NotSupported),
            (
                "stopped",
                &[CONFIG_VALIDATE][..],
                false,
                ServiceCheck::Unreachable,
            ),
            // Attached, but nothing answers before the wait runs out.
            (
                "silent",
                &[CONFIG_VALIDATE][..],
                true,
                ServiceCheck::Unreachable,
            ),
        ] {
            let row = registered(&context, name, capabilities).await;
            let _feed = attached.then(|| context.events().attach(&row.name));

            let report = validate_within(&context, &row, &config, SOON).await;

            assert!(report.valid, "{name}");
            assert_eq!(report.service, expected, "{name}");
        }

        assert_eq!(
            context.validations().pending.lock().len(),
            0,
            "a question nobody answered is withdrawn",
        );
    }

    #[tokio::test]
    async fn a_service_with_a_feed_open_has_the_last_word() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let row = registered(&context, "weather", &[CONFIG_VALIDATE]).await;
        let _feed = context.events().attach(&row.name);
        let refusal = ConfigIssue::at("/api_key", "The upstream refused this key.");
        answering(&context, &row, vec![refusal.clone()]);

        let report = validate(&context, &row, &serde_json::json!({ "api_key": "x" })).await;

        assert_eq!(report.service, ServiceCheck::Checked);
        assert_eq!(report.issues, vec![refusal]);
        assert!(!report.valid);
    }

    #[tokio::test]
    async fn one_service_can_neither_read_nor_answer_anothers_candidate() {
        let validations = Validations::default();
        let (weather, adsb) = (
            ServiceName::parse("weather").unwrap(),
            ServiceName::parse("adsb").unwrap(),
        );
        let asked = validations
            .ask(&weather, serde_json::json!({ "api_key": "hunter2" }))
            .unwrap();

        assert!(validations.candidate(&adsb, asked.id).is_none());
        assert!(!validations.answer(&adsb, asked.id, ConfigValidation::accepted()));
        assert!(validations.candidate(&weather, asked.id).is_some());
        assert!(!format!("{validations:?}").contains("hunter2"));
    }
}
