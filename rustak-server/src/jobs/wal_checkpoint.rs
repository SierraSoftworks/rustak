//! Folding the write-ahead log back into the database, periodically.
//!
//! Under WAL a commit appends to `rustak.sqlite-wal` and the main file is only
//! updated when a checkpoint runs. SQLite checkpoints automatically when the
//! log passes a threshold, but that runs on whichever connection happens to
//! commit at the time — which on a quiet installation may be hours later, and
//! on a busy one is a write waiting behind a fold it did not ask for. Running
//! it on a schedule keeps the log small and keeps the cost off the hot path.
//!
//! `PASSIVE` because that is the mode that never waits: it folds in whatever no
//! reader is still looking at and gives up on the rest, which is exactly right
//! for something running every few minutes. The `TRUNCATE` that leaves the data
//! directory tidy happens once, in [`Database::close`](crate::db::Database).

use chrono::TimeDelta;
use rustak_core::prelude::*;

use crate::{
    db::Checkpoint,
    jobs::{Job, JobContext},
    register_job,
    services::Services,
};

/// The queue partition this job owns.
pub const WAL_CHECKPOINT_PARTITION: &str = "housekeeping/wal-checkpoint";

/// The message this job runs on.
///
/// Empty: the interval is read from the configuration by whoever arms the next
/// run rather than carried here, so a message queued under an old `[storage]
/// checkpoint_interval` does not bake it in. The configuration itself is read
/// once, at start-up — [`Services::config`] hands back what was parsed then,
/// and nothing reloads it — so an edited interval takes effect at the next
/// restart, and *at* it: [`setup`](Job::setup) pulls in a checkpoint armed
/// further out than the new interval, and every run after re-arms with it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalCheckpointTask {}

/// Runs `PRAGMA wal_checkpoint(PASSIVE)` on `[storage] checkpoint_interval`.
pub struct WalCheckpointJob;

register_job!(WalCheckpointJob);

impl Job for WalCheckpointJob {
    type JobType = WalCheckpointTask;

    fn partition() -> &'static str {
        WAL_CHECKPOINT_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// A checkpoint should never be the reason a message is retried; one
    /// interval is plenty.
    fn timeout(&self) -> TimeDelta {
        TimeDelta::minutes(1)
    }

    /// Arms the schedule, the first run one interval out, leaving a checkpoint
    /// that is already armed alone.
    ///
    /// Not immediate, unlike the audit prune: nothing has been written yet, so
    /// a checkpoint at start-up would be work with nothing to do. Not a plain
    /// enqueue either, which would push an armed checkpoint a whole interval
    /// out at every restart; see [`Job::arm_recurring`].
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        let interval = services.config().storage.checkpoint_interval;

        Self::arm_recurring(WalCheckpointTask {}, interval, interval, &services).await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        _job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();
        let interval = services.config().storage.checkpoint_interval;

        // Re-armed before the work, so that a checkpoint which fails is one
        // missed checkpoint rather than the end of the schedule.
        Self::dispatch_delayed(
            WalCheckpointTask {},
            Some(WAL_CHECKPOINT_PARTITION.into()),
            interval,
            &services,
        )
        .await?;

        services.db().checkpoint(Checkpoint::Passive).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{jobs::JobRunnable, prelude::*, services::AppContext};

    #[tokio::test]
    async fn setting_up_arms_the_schedule_one_interval_out() {
        let context = AppContext::new_mock(|config| {
            config.storage.checkpoint_interval = TimeDelta::minutes(5);
        })
        .await
        .unwrap();

        Job::setup(&WalCheckpointJob, context.clone())
            .await
            .unwrap();

        let armed = context
            .queue()
            .peek::<_, WalCheckpointTask>(WAL_CHECKPOINT_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1);
        assert!(
            armed[0].hidden_until > chrono::Utc::now() + TimeDelta::minutes(4),
            "the first checkpoint should be an interval away, not immediate",
        );
    }

    #[tokio::test]
    async fn a_restart_does_not_postpone_a_checkpoint_that_is_already_armed() {
        let context = AppContext::new_mock(|config| {
            config.storage.checkpoint_interval = TimeDelta::minutes(5);
        })
        .await
        .unwrap();
        WalCheckpointJob::dispatch_delayed(
            WalCheckpointTask {},
            Some(WAL_CHECKPOINT_PARTITION.into()),
            TimeDelta::minutes(2),
            &context,
        )
        .await
        .unwrap();
        let armed_for = async || {
            context
                .queue()
                .peek::<_, WalCheckpointTask>(WAL_CHECKPOINT_PARTITION, 10)
                .await
                .unwrap()[0]
                .hidden_until
        };
        let before = armed_for().await;

        Job::setup(&WalCheckpointJob, context.clone())
            .await
            .unwrap();

        assert_eq!(armed_for().await, before);
    }

    #[tokio::test]
    async fn a_run_checkpoints_and_re_arms_itself() {
        let context = AppContext::new_mock(|config| {
            config.storage.checkpoint_interval = TimeDelta::minutes(7);
        })
        .await
        .unwrap();

        JobRunnable::handle(
            &WalCheckpointJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        let armed = context
            .queue()
            .peek::<_, WalCheckpointTask>(WAL_CHECKPOINT_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1, "the run should have re-armed itself");
        assert!(armed[0].hidden_until > chrono::Utc::now() + TimeDelta::minutes(6));
    }

    #[tokio::test]
    async fn re_arming_follows_the_configuration_rather_than_the_message() {
        // The interval is never baked into the message: a run re-arms with
        // what the configuration this process started with says.
        let context = AppContext::new_mock(|config| {
            config.storage.checkpoint_interval = TimeDelta::minutes(1);
        })
        .await
        .unwrap();

        Job::handle(
            &WalCheckpointJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &WalCheckpointTask {},
        )
        .await
        .unwrap();

        let armed = context
            .queue()
            .peek::<_, WalCheckpointTask>(WAL_CHECKPOINT_PARTITION, 10)
            .await
            .unwrap();

        assert!(armed[0].hidden_until < chrono::Utc::now() + TimeDelta::minutes(2));
    }
}
