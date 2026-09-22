//! The CoT garbage collector's schedule.
//!
//! Registered like every other job, so that it is visible in the queue, can be
//! run early by hand, and is reasoned about like the audit prune beside it. The
//! work itself is [`sweep`](crate::cot_store::retention::sweep): whole segment
//! files are unlinked and their index rows deleted, and `cot_latest` loses the
//! rows whose messages went stale long ago. Every limit, and the interval, is
//! `[retention]`'s, read at each run — so an edited file takes effect at the
//! next sweep rather than at the next restart but one.
//!
//! # Why the horizon is approximate, and documented as such
//!
//! History is pruned a segment at a time, and a segment spans a range. The
//! effective horizon is therefore `[retention] cot_history` **plus the tail of
//! the segment that straddles it** — up to one segment's worth of extra
//! history. Trimming inside a file would mean rewriting it, which is the cost
//! the append-only design exists to avoid. Both caps are approximate in the
//! same direction and for the same reason.
//!
//! # A restart never postpones a sweep
//!
//! Enqueueing under the schedule's fixed key *reschedules* the message already
//! there. Arming a full interval out at every start-up — which is what this
//! used to do — therefore meant a server restarted more often than the
//! interval never swept at all. [`setup`](Job::setup) now leaves an armed sweep
//! where it is, unless it is further out than the interval the file now asks
//! for.

use chrono::{TimeDelta, Utc};

use crate::cot_store::retention::{self, Limits};
use crate::prelude::*;
use crate::register_job;

/// The queue partition this job owns.
pub const COT_RETENTION_PARTITION: &str = "housekeeping/cot-retention";

/// How long after start-up the first sweep of an installation runs.
///
/// Not at once: start-up is when the writer is busiest, and an installation
/// in a crash loop should not pay for a scan of the index on every lap. Not a
/// whole interval either, or an installation that came up over its limits
/// stays over them for as long as the interval is.
pub const FIRST_SWEEP_DELAY: TimeDelta = TimeDelta::minutes(5);

/// The message this job runs on. The limits come from the configuration at run
/// time, so there is nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CotRetentionTask {}

/// Keeps CoT history and `cot_latest` inside `[retention]`.
pub struct CotRetentionJob;

register_job!(CotRetentionJob);

impl CotRetentionJob {
    /// Arms (or re-arms) the one scheduled sweep, `delay` from now.
    async fn arm(delay: TimeDelta, services: &impl Services) -> Result<(), Error> {
        Self::dispatch_delayed(
            CotRetentionTask {},
            Some(COT_RETENTION_PARTITION.into()),
            delay,
            services,
        )
        .await
    }
}

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
        let interval = services.config().retention.cot_sweep_interval;

        let armed = services
            .queue()
            .peek::<_, CotRetentionTask>(COT_RETENTION_PARTITION, 1)
            .await?;

        // Further out than the interval means the interval was shortened since
        // it was armed, and the operator who shortened it is waiting.
        if armed
            .first()
            .is_some_and(|sweep| sweep.hidden_until <= Utc::now() + interval)
        {
            return Ok(());
        }

        Self::arm(interval.min(FIRST_SWEEP_DELAY), &services).await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        _job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();
        let config = services.config();

        // Re-armed before the work, so a sweep that fails is one missed sweep
        // rather than the end of the schedule.
        Self::arm(config.retention.cot_sweep_interval, services).await?;

        let swept = retention::sweep(
            services.db(),
            &config.streams_dir(),
            Limits::from_config(&config.retention, Utc::now()),
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

    use crate::cot_store::STREAM_KIND;
    use crate::store::append_log::{AppendLog, AppendLogOptions};

    /// A context whose streams live under `directory`, sweeping on `interval`.
    async fn context(directory: &tempfile::TempDir, interval: TimeDelta) -> AppContext {
        let path = directory.path().to_path_buf();

        AppContext::new_mock(move |config| {
            *config = crate::config::Config::testing(path);
            config.retention.cot_sweep_interval = interval;
        })
        .await
        .unwrap()
    }

    /// When the one scheduled sweep becomes due.
    async fn armed_for(context: &AppContext) -> chrono::DateTime<Utc> {
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
        let directory = tempfile::tempdir().unwrap();
        let context = context(&directory, TimeDelta::hours(6)).await;

        Job::setup(&CotRetentionJob, context.clone()).await.unwrap();

        let due = armed_for(&context).await;
        assert!(due > Utc::now(), "not during start-up itself");
        assert!(due <= Utc::now() + FIRST_SWEEP_DELAY);
    }

    #[tokio::test]
    async fn a_restart_does_not_postpone_a_sweep_that_is_already_armed() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(&directory, TimeDelta::hours(1)).await;
        CotRetentionJob::arm(TimeDelta::minutes(40), &context)
            .await
            .unwrap();
        let before = armed_for(&context).await;

        Job::setup(&CotRetentionJob, context.clone()).await.unwrap();

        assert_eq!(armed_for(&context).await, before);
    }

    #[tokio::test]
    async fn a_shortened_interval_pulls_in_a_sweep_armed_under_the_old_one() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(&directory, TimeDelta::hours(1)).await;
        CotRetentionJob::arm(TimeDelta::hours(6), &context)
            .await
            .unwrap();

        Job::setup(&CotRetentionJob, context.clone()).await.unwrap();

        assert!(armed_for(&context).await <= Utc::now() + FIRST_SWEEP_DELAY);
    }

    #[tokio::test]
    async fn a_run_removes_expired_history_and_re_arms_on_the_configured_interval() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(&directory, TimeDelta::minutes(30)).await;
        let streams = context.config().streams_dir();

        let mut log = AppendLog::open(
            context.db(),
            &streams,
            STREAM_KIND,
            "ICAO-GONE",
            AppendLogOptions::default(),
        )
        .await
        .unwrap();
        log.append(Utc::now() - TimeDelta::days(30), b"ancient")
            .await
            .unwrap();
        log.seal().await.unwrap();

        Job::handle(
            &CotRetentionJob,
            JobContext::new(context.clone(), Utc::now(), None, None),
            &CotRetentionTask {},
        )
        .await
        .unwrap();

        let kept = context
            .db()
            .stream_segments()
            .expired_before(Utc::now())
            .await
            .unwrap();
        assert!(kept.is_empty(), "the month-old segment should be gone");

        let due = armed_for(&context).await;
        assert!(due > Utc::now() + TimeDelta::minutes(29));
        assert!(due <= Utc::now() + TimeDelta::minutes(30));
    }

    #[tokio::test]
    async fn a_sweep_with_nothing_to_remove_is_not_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(&directory, TimeDelta::hours(1)).await;

        Job::handle(
            &CotRetentionJob,
            JobContext::new(context.clone(), Utc::now(), None, None),
            &CotRetentionTask {},
        )
        .await
        .unwrap();
    }
}
