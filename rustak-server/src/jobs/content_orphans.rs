//! Collecting content-store blobs nothing refers to, on a schedule.
//!
//! `[retention] content_orphans` has documented this delay since the first
//! configuration file was written; this is the collector it was documenting.
//! Without one, two things leak for the life of the volume: blobs whose last
//! referencing row is gone — a deleted package, a purged mission — and partly
//! written uploads under `<content_dir>/tmp` left by a kill mid-upload. Neither
//! is visible to an operator, because the `resources` table goes on reporting a
//! fraction of what `du` says the store holds.
//!
//! The work is [`crate::store::orphans::sweep`]; this is the
//! schedule around it, registered like every other job so that it is visible in
//! the queue and can be run early by hand.
//!
//! # A restart never postpones a sweep
//!
//! Enqueueing under the schedule's fixed key *reschedules* the message already
//! there. Arming a full interval out at every start-up — which is what this
//! used to do — therefore meant a server restarted more often than hourly never
//! swept at all, which is exactly the server a crash loop leaves partial uploads
//! on. [`setup`](Job::setup) now leaves an armed sweep where it is; see
//! [`Job::arm_recurring`].

use chrono::TimeDelta;
use rustak_core::prelude::*;

use crate::{
    jobs::{Job, JobContext},
    register_job,
    services::Services,
    store::orphans,
};

/// The queue partition this job owns.
pub const CONTENT_ORPHANS_PARTITION: &str = "housekeeping/content-orphans";

/// How often the store is swept.
///
/// Hourly: the sweep is a directory walk and one query, the grace period is
/// what actually decides how long an orphan survives, and an installation that
/// has just deleted a large package should see the space back the same
/// afternoon. Not configurable — `[retention] content_orphans` is, and that is
/// what an operator has an opinion about.
pub const SWEEP_INTERVAL: TimeDelta = TimeDelta::hours(1);

/// The message this job runs on. The grace period comes from the configuration
/// at run time, so there is nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentOrphansTask {}

/// Removes stored blobs and temporary uploads nothing refers to.
pub struct ContentOrphansJob;

register_job!(ContentOrphansJob);

impl Job for ContentOrphansJob {
    type JobType = ContentOrphansTask;

    fn partition() -> &'static str {
        CONTENT_ORPHANS_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Arms the first sweep one interval from now, leaving one that is already
    /// armed alone.
    ///
    /// Not immediately: an installation upgrading into this may have a year of
    /// orphans to walk, and a restart is the worst moment to start unlinking.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        Self::arm_recurring(
            ContentOrphansTask {},
            SWEEP_INTERVAL,
            SWEEP_INTERVAL,
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
            ContentOrphansTask {},
            Some(CONTENT_ORPHANS_PARTITION.into()),
            SWEEP_INTERVAL,
            &services,
        )
        .await?;

        let grace = services.config().retention.content_orphans;
        let content = services.content()?;
        let swept = orphans::sweep(services.db(), &content, grace).await?;

        if swept.is_empty() {
            debug!("Nothing in the content store is unreferenced; nothing to remove.");
        } else {
            info!(
                blobs = swept.blobs,
                bytes = swept.bytes,
                temporary = swept.temporary,
                "Removed stored files that nothing refers to."
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Utc};

    use crate::{db::Queue, services::AppContext};

    /// When the one scheduled sweep becomes due.
    async fn armed_for(context: &AppContext) -> DateTime<Utc> {
        let armed = context
            .queue()
            .peek::<_, ContentOrphansTask>(CONTENT_ORPHANS_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1, "there is only ever one scheduled sweep");
        armed[0].hidden_until
    }

    #[test]
    fn the_partition_is_the_one_the_registry_dispatches_on() {
        assert_eq!(ContentOrphansJob::partition(), CONTENT_ORPHANS_PARTITION);
    }

    #[tokio::test]
    async fn the_first_sweep_is_not_at_the_instant_of_start_up() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&ContentOrphansJob, context.clone())
            .await
            .unwrap();

        let due = armed_for(&context).await;
        assert!(due > Utc::now());
        assert!(due <= Utc::now() + SWEEP_INTERVAL);
    }

    #[tokio::test]
    async fn a_restart_does_not_postpone_a_sweep_that_is_already_armed() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        ContentOrphansJob::dispatch_delayed(
            ContentOrphansTask {},
            Some(CONTENT_ORPHANS_PARTITION.into()),
            TimeDelta::minutes(40),
            &context,
        )
        .await
        .unwrap();
        let before = armed_for(&context).await;

        Job::setup(&ContentOrphansJob, context.clone())
            .await
            .unwrap();

        assert_eq!(armed_for(&context).await, before);
    }
}
