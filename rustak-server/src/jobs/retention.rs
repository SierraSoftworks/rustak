//! Keeping CoT history inside `[retention]`, on a schedule.
//!
//! Registered like every other job, so that it is visible in the queue, can be
//! run early by hand, and is reasoned about like the audit prune beside it. The
//! work itself is [`sweep`](crate::cot_store::retention::sweep): whole segment files are
//! unlinked and their index rows deleted, and `cot_latest` loses the rows whose
//! messages went stale long ago.
//!
//! # Why the horizon is approximate, and documented as such
//!
//! History is pruned a segment at a time, and a segment spans a range. The
//! effective horizon is therefore `[retention] cot_history` **plus the tail of
//! the segment that straddles it** — up to one segment's worth of extra
//! history. Trimming inside a file would mean rewriting it, which is the cost
//! the append-only design exists to avoid.
//!
//! The same pass enforces `[retention] cot_history_max_rows`, the per-device
//! floor that keeps a busy installation from filling the disk inside the age
//! window. It is approximate in the same direction and for the same reason.
//!
//! # A restart never postpones a sweep
//!
//! Enqueueing under the schedule's fixed key *reschedules* the message already
//! there. Arming a full interval out at every start-up — which is what this
//! used to do — therefore meant a server restarted more often than the
//! interval never swept at all. [`setup`](Job::setup) now leaves an armed sweep
//! where it is, unless it is further out than the interval; see
//! [`Job::arm_recurring`].

use chrono::TimeDelta;

use crate::cot_store::retention;
use crate::prelude::*;
use crate::register_job;

/// The queue partition this job owns.
pub const COT_RETENTION_PARTITION: &str = "housekeeping/cot-retention";

/// How often the history is trimmed.
///
/// Six hours rather than daily: a segment is eight megabytes and a busy
/// installation fills them quickly, so an installation that has just had its
/// horizon shortened should see the space back the same day. Not configurable —
/// the horizon is, and that is what an operator has an opinion about.
pub const SWEEP_INTERVAL: TimeDelta = TimeDelta::hours(6);

/// How long after start-up the first sweep of an installation runs.
///
/// Not at once: start-up is when the writer is busiest, and an installation
/// in a crash loop should not pay for a scan of the index on every lap. Not a
/// whole interval either, or an installation that came up over its limits
/// stays over them for as long as the interval is.
pub const FIRST_SWEEP_DELAY: TimeDelta = TimeDelta::minutes(5);

/// The message this job runs on. The horizon comes from the configuration at
/// run time, so there is nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CotRetentionTask {}

/// Trims CoT history to `[retention] cot_history`.
pub struct CotRetentionJob;

register_job!(CotRetentionJob);

impl Job for CotRetentionJob {
    type JobType = CotRetentionTask;

    fn partition() -> &'static str {
        COT_RETENTION_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Arms the first sweep, leaving one that is already armed alone.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        Self::arm_recurring(
            CotRetentionTask {},
            SWEEP_INTERVAL,
            FIRST_SWEEP_DELAY,
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

        // Re-armed before the work, so a sweep that fails is one missed sweep
        // rather than the end of the schedule.
        Self::dispatch_delayed(
            CotRetentionTask {},
            Some(COT_RETENTION_PARTITION.into()),
            SWEEP_INTERVAL,
            &services,
        )
        .await?;

        let config = services.config();
        let before = chrono::Utc::now() - config.retention.cot_history;

        let swept = retention::sweep(
            services.db(),
            &config.streams_dir(),
            before,
            config.retention.cot_history_max_rows,
        )
        .await?;

        if swept.is_empty() {
            debug!("The CoT history is within its retention; nothing to remove.");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Utc};

    /// Arms the one scheduled sweep `delay` from now, as an earlier run did.
    async fn arm(delay: TimeDelta, context: &AppContext) {
        CotRetentionJob::dispatch_delayed(
            CotRetentionTask {},
            Some(COT_RETENTION_PARTITION.into()),
            delay,
            context,
        )
        .await
        .unwrap();
    }

    /// When the one scheduled sweep becomes due.
    async fn armed_for(context: &AppContext) -> DateTime<Utc> {
        let armed = context
            .queue()
            .peek::<_, CotRetentionTask>(COT_RETENTION_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1, "there is only ever one scheduled sweep");
        armed[0].hidden_until
    }

    #[tokio::test]
    async fn the_first_sweep_is_soon_after_start_up_rather_than_an_interval_away() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&CotRetentionJob, context.clone()).await.unwrap();

        let due = armed_for(&context).await;
        assert!(due > Utc::now(), "not during start-up itself");
        assert!(due <= Utc::now() + FIRST_SWEEP_DELAY);
    }

    #[tokio::test]
    async fn a_restart_does_not_postpone_a_sweep_that_is_already_armed() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        arm(TimeDelta::minutes(40), &context).await;
        let before = armed_for(&context).await;

        Job::setup(&CotRetentionJob, context.clone()).await.unwrap();

        assert_eq!(armed_for(&context).await, before);
    }

    #[tokio::test]
    async fn a_sweep_armed_further_out_than_the_interval_is_pulled_in() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        arm(SWEEP_INTERVAL * 4, &context).await;

        Job::setup(&CotRetentionJob, context.clone()).await.unwrap();

        assert!(armed_for(&context).await <= Utc::now() + FIRST_SWEEP_DELAY);
    }

    #[test]
    fn the_partition_is_the_one_the_registry_dispatches_on() {
        assert_eq!(CotRetentionJob::partition(), COT_RETENTION_PARTITION);
    }

    #[tokio::test]
    async fn a_sweep_with_nothing_to_remove_is_not_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let context = AppContext::new_mock(move |config| {
            *config = crate::config::Config::testing(path);
        })
        .await
        .unwrap();

        let config = context.config();
        let before = chrono::Utc::now() - config.retention.cot_history;

        let swept = retention::sweep(
            context.db(),
            &config.streams_dir(),
            before,
            config.retention.cot_history_max_rows,
        )
        .await
        .unwrap();

        assert!(swept.is_empty());
    }
}
