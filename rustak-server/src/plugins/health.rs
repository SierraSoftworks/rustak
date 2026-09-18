//! Heartbeats, and deciding that one is overdue.
//!
//! A sidecar says how it is doing on every tick; this records what it said,
//! publishes `service.status` on the server-event feed, and answers the
//! configuration it should be running with — so a plugin's heartbeat is also how
//! it picks up a setting an administrator changed, without a second call and
//! without a restart.
//!
//! # Why silence is a state rather than an absence
//!
//! "Healthy, an hour ago" is the answer that misleads an operator: the row still
//! says healthy, and nothing in the UI says the sidecar stopped talking. [`sweep`]
//! is the counterweight — anything that has not reported within the grace period
//! is moved back to [`ServiceState::Unknown`], which
//! [`needs_attention`](ServiceState::needs_attention) reports as worth looking
//! at. The sweep is the only writer that is not the service itself, which is why
//! it clears the message too: the last thing a service said stops being true
//! when it stops saying it.

use rustak_api::{Heartbeat, ServiceState, ServiceStatus};
use rustak_core::prelude::*;

use crate::db::repos::ServiceRow;
use crate::prelude::*;

/// How long after its last heartbeat a service is taken to have stopped
/// reporting, when nothing says otherwise.
///
/// Three times the sidecar harness's default tick (30s), so a single missed
/// heartbeat — a slow upstream, a restart, a network blip — does not turn the
/// whole fleet amber.
pub const DEFAULT_GRACE: chrono::TimeDelta = chrono::TimeDelta::seconds(90);

/// Records what a service said about itself.
///
/// Answers the status as it now reads, which is what the service is handed back:
/// a sidecar that has just reported degraded sees its own words, which makes a
/// heartbeat that silently did not land obvious in a plugin's own logs.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails. A heartbeat for a
/// registration that has been removed under the service's feet is *not* an
/// error: it answers `false`, and the caller re-registers.
#[instrument("plugins.heartbeat", skip_all, fields(service = %row.name, state = beat.state.as_str()), err(Display))]
pub async fn record(
    services: &impl Services,
    row: &ServiceRow,
    beat: &Heartbeat,
) -> Result<Option<ServiceStatus>, Error> {
    let recorded = services
        .db()
        .services()
        .record_heartbeat(
            row.id,
            beat.state,
            beat.message.clone(),
            beat.metrics.clone(),
        )
        .await?;

    if !recorded {
        return Ok(None);
    }

    services
        .events()
        .service_status(&row.name, beat.state, beat.message.as_deref());

    Ok(Some(ServiceStatus {
        state: beat.state,
        message: beat.message.clone(),
        last_heartbeat_at: Some(chrono::Utc::now()),
    }))
}

/// Moves every service that has stopped reporting back to
/// [`ServiceState::Unknown`], and says how many it moved.
///
/// Announced on the feed one event per service, because a consumer that reacts
/// to a sidecar going quiet — paging somebody, failing over — needs to know
/// *which* one, and a single "n services went quiet" event would make it read
/// the whole listing to find out.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or the write fails.
#[instrument("plugins.sweep", skip_all, err(Display))]
pub async fn sweep(services: &impl Services, grace: chrono::TimeDelta) -> Result<usize, Error> {
    let cutoff = chrono::Utc::now() - grace;
    let overdue: Vec<ServiceRow> = services
        .db()
        .services()
        .list()
        .await?
        .into_iter()
        .filter(|row| is_overdue(row, cutoff))
        .collect();

    if overdue.is_empty() {
        return Ok(0);
    }

    let swept = services.db().services().mark_silent_before(cutoff).await?;

    for row in &overdue {
        info!(service = %row.name, "A service has stopped reporting.");
        services
            .events()
            .service_status(&row.name, ServiceState::Unknown, None);
    }

    Ok(swept)
}

/// Whether this row is one [`sweep`] would move.
///
/// Mirrors the repository's `mark_silent_before` predicate so that the events
/// published and the rows written are the same set; a service that is already
/// `unknown` is neither swept nor announced again.
fn is_overdue(row: &ServiceRow, cutoff: chrono::DateTime<chrono::Utc>) -> bool {
    row.status != ServiceState::Unknown && row.last_heartbeat_at.is_none_or(|last| last < cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::{NewService, NewUser};

    async fn registered(context: &AppContext) -> ServiceRow {
        let account = context
            .db()
            .users()
            .create(NewUser::service(Username::parse("svc.weather").unwrap()))
            .await
            .unwrap();

        context
            .db()
            .services()
            .register(NewService::new(
                ServiceName::parse("weather").unwrap(),
                account.id,
            ))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_heartbeat_is_recorded_and_announced() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let row = registered(&context).await;

        let status = record(
            &context,
            &row,
            &Heartbeat {
                state: ServiceState::Degraded,
                message: Some("Upstream is slow.".into()),
                metrics: serde_json::json!({ "queue_depth": 4 }),
            },
        )
        .await
        .unwrap()
        .expect("the registration is still there");

        assert_eq!(status.state, ServiceState::Degraded);
        assert!(status.last_heartbeat_at.is_some());

        let read = context.db().services().get(row.id).await.unwrap().unwrap();
        assert_eq!(read.status, ServiceState::Degraded);
        assert_eq!(read.metrics, serde_json::json!({ "queue_depth": 4 }));

        let published = context.events().since(0);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].event.name(), "service.status");
    }

    #[tokio::test]
    async fn a_heartbeat_for_a_registration_that_has_gone_is_not_a_failure() {
        // An administrator can remove a registration while the sidecar is
        // running; the sidecar finds out on its next heartbeat and registers
        // again rather than exiting.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let row = registered(&context).await;
        context.db().services().delete(row.id).await.unwrap();

        let status = record(&context, &row, &Heartbeat::healthy()).await.unwrap();

        assert!(status.is_none());
        assert!(context.events().since(0).is_empty(), "nothing to announce");
    }

    #[tokio::test]
    async fn a_service_that_stops_reporting_is_swept_once() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let row = registered(&context).await;
        record(&context, &row, &Heartbeat::healthy()).await.unwrap();

        // A grace period that has already elapsed, rather than a sleep.
        let swept = sweep(&context, chrono::TimeDelta::seconds(-1))
            .await
            .unwrap();

        assert_eq!(swept, 1);
        let read = context.db().services().get(row.id).await.unwrap().unwrap();
        assert_eq!(read.status, ServiceState::Unknown);
        assert!(read.status_message.is_none());

        // Once. A service that is already quiet is not re-announced on every
        // sweep, or a feed consumer would see one event per minute forever.
        assert_eq!(
            sweep(&context, chrono::TimeDelta::seconds(-1))
                .await
                .unwrap(),
            0
        );
        assert_eq!(context.events().since(0).len(), 2, "the beat and the sweep");
    }

    #[tokio::test]
    async fn a_service_still_within_its_grace_period_is_left_alone() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let row = registered(&context).await;
        record(&context, &row, &Heartbeat::healthy()).await.unwrap();

        assert_eq!(sweep(&context, DEFAULT_GRACE).await.unwrap(), 0);
        assert_eq!(
            context
                .db()
                .services()
                .get(row.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ServiceState::Healthy
        );
    }
}
