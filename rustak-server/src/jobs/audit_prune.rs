//! Trimming the audit log back to what `[retention]` allows.
//!
//! Lifted from automate's `JobHost::prune_audit_log`, as a registered job
//! rather than a task the host spawns for itself: rustak has a job registry, so
//! housekeeping that runs on a schedule belongs in it, where it can be seen in
//! the queue, run early by hand and reasoned about like everything else.
//!
//! The log is append-only and read by people, so it needs a bound whatever is
//! written to it — how much is written decides how fast it fills, not whether.
//! Both limits in `[retention]` apply, because either alone leaves a gap: an age
//! limit lets a busy day fill the disk inside the window, and a count limit lets
//! a quiet installation keep entries for ever.

use chrono::TimeDelta;
use rustak_core::prelude::*;

use crate::{
    db::AuditStore,
    jobs::{Job, JobContext},
    register_job,
    services::Services,
};

/// The queue partition this job owns.
pub const AUDIT_PRUNE_PARTITION: &str = "housekeeping/audit-prune";

/// How often the log is trimmed.
///
/// Often enough that it never drifts far past its limits, and rarely enough
/// that the delete is never the reason another write is waiting. Not
/// configurable: the limits are, and they are what an operator actually has an
/// opinion about.
pub const PRUNE_INTERVAL: TimeDelta = TimeDelta::hours(24);

/// The message this job runs on. The limits come from the configuration at run
/// time, so there is nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditPruneTask {}

/// Trims the audit log to `[retention] audit` and `[retention]
/// audit_max_entries`, daily.
pub struct AuditPruneJob;

register_job!(AuditPruneJob);

impl Job for AuditPruneJob {
    type JobType = AuditPruneTask;

    fn partition() -> &'static str {
        AUDIT_PRUNE_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Arms the schedule to run at once.
    ///
    /// Immediate, unlike the checkpoint: an installation that has been running
    /// with the wrong limits, or upgrading into this, should not have to wait a
    /// day for its log to come back inside them.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        Self::dispatch(
            AuditPruneTask {},
            Some(AUDIT_PRUNE_PARTITION.into()),
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

        // Re-armed before the work, so that a prune which fails is one missed
        // prune rather than the end of the schedule. A failure is not retried
        // sooner on purpose: a log that is one day too long is not a reason to
        // keep hammering the database.
        Self::dispatch_delayed(
            AuditPruneTask {},
            Some(AUDIT_PRUNE_PARTITION.into()),
            PRUNE_INTERVAL,
            &services,
        )
        .await?;

        let retention = &services.config().retention;
        let max_entries = usize::try_from(retention.audit_max_entries).unwrap_or(usize::MAX);

        let removed = services
            .audit()
            .prune_audit_log(retention.audit, max_entries)
            .await?;

        if removed == 0 {
            debug!("The audit log is within its retention; nothing to remove.");
        } else {
            info!(
                audit.removed = removed,
                "Removed {removed} audit entries past their retention."
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rustak_api::{AuditCategory, AuditOutcome};

    use crate::{
        db::{AuditEntry, AuditQuery},
        jobs::JobRunnable,
        prelude::*,
        services::AppContext,
    };

    /// Writes `count` entries, which the prune then has something to remove.
    async fn seed(context: &AppContext, count: usize) {
        for index in 0..count {
            context
                .audit()
                .record(
                    AuditEntry::new(AuditCategory::System, "started", AuditOutcome::Success)
                        .subject(format!("entry-{index}")),
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn setting_up_arms_the_schedule_to_run_at_once() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&AuditPruneJob, context.clone()).await.unwrap();

        let due = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(30))
            .await
            .unwrap()
            .expect("the prune should be due immediately");

        assert_eq!(due.partition, AUDIT_PRUNE_PARTITION);
    }

    #[tokio::test]
    async fn a_run_trims_the_log_to_the_configured_count_and_re_arms() {
        let context = AppContext::new_mock(|config| {
            config.retention.audit_max_entries = 3;
        })
        .await
        .unwrap();
        seed(&context, 10).await;

        JobRunnable::handle(
            &AuditPruneJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        let kept = context.audit().audit(AuditQuery::recent(50)).await.unwrap();
        assert_eq!(kept.len(), 3, "the newest three entries should have stayed");
        assert_eq!(kept[0].subject.as_deref(), Some("entry-9"));

        let armed = context
            .queue()
            .peek::<_, AuditPruneTask>(AUDIT_PRUNE_PARTITION, 10)
            .await
            .unwrap();
        assert_eq!(armed.len(), 1, "the run should have re-armed itself");
        assert!(armed[0].hidden_until > chrono::Utc::now() + TimeDelta::hours(23));
    }

    #[tokio::test]
    async fn a_log_inside_its_retention_is_left_alone() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        seed(&context, 5).await;

        Job::handle(
            &AuditPruneJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &AuditPruneTask {},
        )
        .await
        .unwrap();

        let kept = context.audit().audit(AuditQuery::recent(50)).await.unwrap();
        assert_eq!(kept.len(), 5);
    }
}
