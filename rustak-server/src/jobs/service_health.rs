//! Noticing that a sidecar has stopped reporting.
//!
//! A service's status is whatever it last said about itself, which means the
//! answer that misleads an operator is "healthy, an hour ago": the row still
//! reads healthy and nothing says the process died. This job is the
//! counterweight — anything that has not reported within
//! [`DEFAULT_GRACE`](crate::plugins::health::DEFAULT_GRACE) is moved back to
//! `unknown`, which the admin UI draws as needing attention and which
//! `service.status` announces on the server-event feed.
//!
//! Self-arming on the shape `audit_prune` established: it re-arms before it
//! works, so a sweep that fails is one missed sweep rather than the end of the
//! schedule.

use chrono::TimeDelta;
use rustak_core::prelude::*;

use crate::{
    jobs::{Job, JobContext},
    plugins::health,
    register_job,
    services::Services,
};

/// The queue partition this job owns.
pub const SERVICE_HEALTH_PARTITION: &str = "housekeeping/service-health";

/// How often the registrations are swept.
///
/// A third of the grace period, so a sidecar that stops is reported as quiet
/// within about one extra tick of the harness's default rather than within a
/// whole grace period of it.
pub const SERVICE_HEALTH_INTERVAL: TimeDelta = TimeDelta::seconds(30);

/// The message this job runs on. The grace period is a constant, so there is
/// nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceHealthTask {}

/// Moves every service that has stopped reporting back to `unknown`.
pub struct ServiceHealthJob;

register_job!(ServiceHealthJob);

impl Job for ServiceHealthJob {
    type JobType = ServiceHealthTask;

    fn partition() -> &'static str {
        SERVICE_HEALTH_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Arms the schedule to run at once.
    ///
    /// Immediately, because a restart is exactly when the statuses on disk are
    /// least likely to be true: every sidecar that was healthy when this server
    /// stopped still reads healthy, and none of them has reported since.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        Self::dispatch(
            ServiceHealthTask {},
            Some(SERVICE_HEALTH_PARTITION.into()),
            &services,
        )
        .await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        _job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();

        // Re-armed before the work, for the reason `audit_prune` gives.
        Self::dispatch_delayed(
            ServiceHealthTask {},
            Some(SERVICE_HEALTH_PARTITION.into()),
            SERVICE_HEALTH_INTERVAL,
            &services,
        )
        .await?;

        let swept = health::sweep(&services, health::DEFAULT_GRACE).await?;

        if swept > 0 {
            info!(
                services.swept = swept,
                "{swept} service(s) have stopped reporting."
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::{Heartbeat, ServiceState};

    use super::*;
    use crate::db::repos::{NewService, NewUser};
    use crate::prelude::*;
    use crate::services::AppContext;

    /// A registered service that has just said it is healthy.
    async fn healthy(context: &AppContext) -> ServiceId {
        let account = context
            .db()
            .users()
            .create(NewUser::service(Username::parse("svc.weather").unwrap()))
            .await
            .unwrap();
        let row = context
            .db()
            .services()
            .register(NewService::new(
                ServiceName::parse("weather").unwrap(),
                account.id,
            ))
            .await
            .unwrap();

        health::record(context, &row, &Heartbeat::healthy())
            .await
            .unwrap();

        row.id
    }

    #[tokio::test]
    async fn setting_up_arms_the_schedule_to_run_at_once() {
        // A restart is when the stored statuses are least likely to be true:
        // every sidecar that was healthy when this server stopped still reads
        // healthy, and none of them has reported since.
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&ServiceHealthJob, context.clone())
            .await
            .unwrap();

        let due = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(30))
            .await
            .unwrap()
            .expect("the sweep should be due immediately");

        assert_eq!(due.partition, SERVICE_HEALTH_PARTITION);
    }

    #[tokio::test]
    async fn a_service_reporting_within_its_grace_period_is_left_alone() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let id = healthy(&context).await;

        Job::handle(
            &ServiceHealthJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &ServiceHealthTask {},
        )
        .await
        .unwrap();

        assert_eq!(
            context
                .db()
                .services()
                .get(id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ServiceState::Healthy,
        );
    }

    #[tokio::test]
    async fn a_run_re_arms_itself_before_it_works() {
        // The property the schedule rests on: a sweep that fails is one missed
        // sweep rather than the end of the schedule.
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::handle(
            &ServiceHealthJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &ServiceHealthTask {},
        )
        .await
        .unwrap();

        let armed = context
            .queue()
            .peek::<_, ServiceHealthTask>(SERVICE_HEALTH_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1);
        assert!(armed[0].hidden_until > chrono::Utc::now());
    }
}
